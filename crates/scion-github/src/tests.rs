use super::*;
use scion_core::Outcome;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    methods: Arc<Mutex<Vec<String>>>,
}
impl Server {
    fn new(handler: impl Fn(&str, usize) -> (u16, Value) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/", listener.local_addr().expect("addr"));
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let methods = Arc::new(Mutex::new(Vec::new()));
        let recorded = methods.clone();
        let handle = thread::spawn(move || {
            let mut counts = std::collections::BTreeMap::<String, usize>::new();
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).expect("blocking stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("timeout");
                let mut bytes = Vec::new();
                let mut one = [0];
                while bytes.len() < 16384 && !bytes.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut one).unwrap_or(0) == 0 {
                        break;
                    }
                    bytes.push(one[0]);
                }
                let request = String::from_utf8_lossy(&bytes);
                let mut first = request.lines().next().unwrap_or("").split_whitespace();
                recorded
                    .lock()
                    .expect("methods")
                    .push(first.next().unwrap_or("").into());
                let path = first.next().unwrap_or("");
                let count = counts.entry(path.into()).or_default();
                *count += 1;
                let (status, body) = handler(path, *count);
                let body = body.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url,
            stop,
            handle: Some(handle),
            methods,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().expect("thread").join().expect("join");
    }
}

fn fixture(path: &str, _count: usize) -> (u16, Value) {
    if path.contains("/statuses?") {
        return (200, json!([]));
    }
    if path == "/repos/example/project/pulls/42" {
        return (
            200,
            json!({"number":42,"state":"open","merged":false,"draft":false,"merge_commit_sha":"c".repeat(40),"user":{"id":1},"head":{"sha":"a".repeat(40)},"base":{"sha":"b".repeat(40),"ref":"main","repo":{"full_name":"example/project"}}}),
        );
    }
    if path == "/repos/example/project/git/ref/heads/main" {
        return (200, json!({"object":{"sha":"b".repeat(40)}}));
    }
    if path.contains("check-runs") {
        return (
            200,
            json!({"check_runs":[{"id":1,"name":"test","app":{"id":15368},"head_sha":"a".repeat(40),"status":"completed","conclusion":"success","html_url":"https://example.invalid/run/1"}]}),
        );
    }
    if path.contains("reviews?") {
        return (
            200,
            json!([{"id":1,"user":{"id":2},"state":"APPROVED","commit_id":"a".repeat(40)}]),
        );
    }
    if path.ends_with("required_status_checks") {
        return (
            200,
            json!({"contexts":["test"],"checks":[{"context":"test","app_id":15368}]}),
        );
    }
    if path.contains("rules/branches") {
        return (200, json!([]));
    }
    (404, json!({"message":"unexpected test route"}))
}

#[test]
fn real_get_transport_collects_bound_check_and_review_metadata() {
    let server = Server::new(fixture);
    let observation = GitHubClient::with_base(&server.url, None)
        .expect("client")
        .observe("example/project", 42)
        .expect("observe PR");
    assert!(observation.stable);
    assert_eq!(observation.head_ci, Outcome::Pass);
    assert_eq!(observation.review_policy, Outcome::NotRun);
    assert_eq!(observation.checks[0].app_id, 15368);
    assert!(
        server
            .methods
            .lock()
            .expect("methods")
            .iter()
            .all(|m| m == "GET")
    );
}

#[test]
fn a_public_approval_does_not_authenticate_review_policy() {
    let server = Server::new(fixture);
    let observation = GitHubClient::with_base(&server.url, None)
        .expect("client")
        .observe("example/project", 42)
        .expect("observe");
    assert_eq!(observation.review_policy, Outcome::NotRun);
    assert_eq!(observation.reviews.len(), 1);
}

#[test]
fn same_name_check_from_another_app_cannot_hide_a_real_failure() {
    let server = Server::new(|path, count| {
        let (status, mut value) = fixture(path, count);
        if path.contains("check-runs") {
            value["check_runs"][0]["conclusion"] = "failure".into();
            let mut fake = value["check_runs"][0].clone();
            fake["id"] = 2.into();
            fake["app"]["id"] = 999.into();
            fake["conclusion"] = "success".into();
            value["check_runs"]
                .as_array_mut()
                .expect("checks")
                .push(fake);
        }
        (status, value)
    });
    let observation = GitHubClient::with_base(&server.url, None)
        .expect("client")
        .observe("example/project", 42)
        .expect("observe");
    assert_eq!(observation.head_ci, Outcome::Fail);
}

#[test]
fn a_moved_head_or_live_base_invalidates_the_poll() {
    for move_head in [true, false] {
        let server = Server::new(move |path, count| {
            let (status, mut value) = fixture(path, count);
            if count == 2 && move_head && path.ends_with("pulls/42") {
                value["head"]["sha"] = "d".repeat(40).into();
            }
            if count == 2 && !move_head && path.ends_with("git/ref/heads/main") {
                value["object"]["sha"] = "d".repeat(40).into();
            }
            (status, value)
        });
        let observation = GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42)
            .expect("observe");
        assert!(!observation.stable);
        assert_eq!(observation.head_ci, Outcome::Blocked);
    }
}

#[test]
fn missing_policy_sources_and_non_success_conclusions_block() {
    for scenario in 0..5 {
        let server = Server::new(move |path, count| {
            let (status, mut value) = fixture(path, count);
            if path.ends_with("required_status_checks") {
                if scenario == 0 {
                    return (403, json!({"message":"denied"}));
                }
                if scenario == 1 {
                    return (404, json!({"message":"Branch not protected"}));
                }
                if scenario == 2 {
                    value["checks"][0]["app_id"] = Value::Null;
                }
            }
            if path.contains("check-runs") && scenario >= 3 {
                value["check_runs"][0]["conclusion"] = if scenario == 3 {
                    "skipped".into()
                } else {
                    "neutral".into()
                };
            }
            (status, value)
        });
        let observation = GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42)
            .expect("observe");
        assert_eq!(observation.head_ci, Outcome::Blocked);
    }
}

#[test]
fn paginated_reviews_are_fully_collected() {
    let server = Server::new(|path, count| {
        if path.contains("reviews?") {
            if path.contains("page=2") {
                return (
                    200,
                    json!([{"id":101,"user":{"id":102},"state":"COMMENTED","commit_id":"a".repeat(40)}]),
                );
            }
            return (200,Value::Array((1..=100).map(|id|json!({"id":id,"user":{"id":id+1},"state":"COMMENTED","commit_id":"a".repeat(40)})).collect()));
        }
        fixture(path, count)
    });
    let observation = GitHubClient::with_base(&server.url, None)
        .expect("client")
        .observe("example/project", 42)
        .expect("observe");
    assert_eq!(observation.reviews.len(), 101);
}

#[test]
fn invalid_target_is_rejected_before_any_request() {
    let server = Server::new(fixture);
    let client = GitHubClient::with_base(&server.url, None).expect("client");
    assert!(matches!(
        client.observe("../project", 42),
        Err(GitHubError::InvalidInput)
    ));
    assert!(matches!(
        client.observe("example/project", 0),
        Err(GitHubError::InvalidInput)
    ));
    assert!(server.methods.lock().expect("methods").is_empty());
}

#[test]
fn active_ruleset_and_classic_requirements_are_both_required() {
    for include_other_check in [false, true] {
        let server = Server::new(move |path, count| {
            if path.contains("rules/branches") {
                return (
                    200,
                    json!([{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"other","integration_id":15368}]}}]),
                );
            }
            let (status, mut value) = fixture(path, count);
            if path.contains("check-runs") && include_other_check {
                let mut other = value["check_runs"][0].clone();
                other["id"] = 2.into();
                other["name"] = "other".into();
                value["check_runs"]
                    .as_array_mut()
                    .expect("checks")
                    .push(other);
            }
            (status, value)
        });
        let result = GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42)
            .expect("observe");
        assert_eq!(result.required_checks.len(), 2);
        assert_eq!(
            result.head_ci,
            if include_other_check {
                Outcome::Pass
            } else {
                Outcome::Blocked
            }
        );
    }
}

#[test]
fn pagination_limit_is_an_error_instead_of_a_partial_success() {
    let server = Server::new(|path, count| {
        if path.contains("check-runs") {
            let (_, value) = fixture(path, count);
            return (
                200,
                json!({"check_runs":vec![value["check_runs"][0].clone();100]}),
            );
        }
        fixture(path, count)
    });
    assert!(matches!(
        GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42),
        Err(GitHubError::Limit)
    ));
}

#[test]
fn legacy_status_on_a_required_context_cannot_be_hidden_by_a_passing_check_run() {
    for required_context in [true, false] {
        let server = Server::new(move |path, count| {
            if path.contains("/statuses?") {
                return (
                    200,
                    json!([{"id":8,"context":if required_context{"test"}else{"unrelated"},"state":"failure"}]),
                );
            }
            fixture(path, count)
        });
        let observation = GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42)
            .expect("observe");
        assert_eq!(
            observation.head_ci,
            if required_context {
                Outcome::Blocked
            } else {
                Outcome::Pass
            }
        );
    }
}

#[test]
fn unavailable_legacy_statuses_cannot_be_assumed_empty() {
    let server = Server::new(|path, count| {
        if path.contains("/statuses?") {
            return (403, json!({"message":"denied"}));
        }
        fixture(path, count)
    });
    assert!(matches!(
        GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42),
        Err(GitHubError::Http(403))
    ));
}

#[test]
fn historical_reviews_with_null_author_or_commit_remain_observable() {
    for missing_author in [true, false] {
        let server = Server::new(move |path, count| {
            let (status, mut value) = fixture(path, count);
            if path.contains("reviews?") {
                if missing_author {
                    value[0]["user"] = Value::Null;
                } else {
                    value[0]["commit_id"] = Value::Null;
                }
            }
            (status, value)
        });
        let observation = GitHubClient::with_base(&server.url, None)
            .expect("client")
            .observe("example/project", 42)
            .expect("historical review");
        let value = serde_json::to_value(&observation.reviews[0]).expect("review");
        assert!(
            value[if missing_author {
                "author_id"
            } else {
                "commit_sha"
            }]
            .is_null()
        );
        assert_eq!(observation.review_policy, Outcome::NotRun);
    }
}
