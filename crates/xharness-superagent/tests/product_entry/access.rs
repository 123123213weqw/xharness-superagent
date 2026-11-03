//! Real formal entry + native filesystem/process tools. Only the network peer
//! and model/GitHub services are fixtures; no host credentials or paid calls.
use super::*;

#[tokio::test]
async fn explicit_full_access_worker_can_use_network_and_owned_external_path() {
    let peer = TcpListener::bind("127.0.0.1:0").unwrap();
    peer.set_nonblocking(true).unwrap();
    let url = format!("http://{}/access-probe", peer.local_addr().unwrap());
    let network = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        loop {
            match peer.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut buf = [0; 4096];
                    let n = stream.read(&mut buf).unwrap();
                    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /access-probe "));
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\nNETWORK_ALLOWED").unwrap();
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Worker never reached the network peer"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("network peer: {e}"),
            }
        }
    });
    let external = Arc::new(Mutex::new(PathBuf::new()));
    let target = external.clone();
    let server = Server::with_responder(move |request| {
        if request["model"] != "worker" {
            return answer(request, false);
        }
        Ok(match calls(request).len() {
            0 => (
                Some("bash"),
                json!({"program":"python3","args":["-c",
                "import os,sys,urllib.request; assert 'SUPERAGENT_FIXTURE_API_KEY' not in os.environ; assert urllib.request.urlopen(sys.argv[1],timeout=5).read()==b'NETWORK_ALLOWED'; print('NETWORK_AND_SECRET_SCRUB_OK')",url]}),
            ),
            1 => (
                Some("write"),
                json!({"path":target.lock().unwrap().to_str().unwrap(),"content":"FULL_ACCESS 😀\n"}),
            ),
            2 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Full access probe completed."}),
            ),
            _ => (None, Value::Null),
        })
    });
    let mut fixture = Fixture::new(&server.url);
    let path = fixture.directory.join("outside-worker-workspace.txt");
    *external.lock().unwrap() = path.clone();
    fixture.config["workerAccess"] = json!("danger-full-access");
    success(&fixture.run("--once").await);
    network.join().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "FULL_ACCESS 😀\n");
    let requests = server.requests();
    let worker: Vec<_> = requests.iter().filter(|r| r["model"] == "worker").collect();
    assert!(!worker.is_empty());
    assert!(worker.iter().all(|r| r["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "web_fetch")));
    assert!(worker
        .iter()
        .flat_map(|r| r["messages"].as_array().unwrap())
        .any(|m| m["role"] == "tool" && message_text(m).contains("NETWORK_AND_SECRET_SCRUB_OK")));
    for request in requests.iter().filter(|r| r["model"] == "main") {
        assert!(!request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "bash"));
    }
    let sessions = std::fs::read_dir(fixture.directory.join("state/sessions")).unwrap();
    assert!(sessions
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|e| e == "jsonl"))
        .any(|e| std::fs::read_to_string(e.path())
            .unwrap()
            .contains("danger-full-access")));
}

#[tokio::test]
async fn unknown_worker_access_is_rejected_before_product_start() {
    let mut fixture = Fixture::new("http://127.0.0.1:1/v1");
    fixture.config["workerAccess"] = json!("fullaccess-typo");
    let output = fixture.run("--check").await;
    assert!(!output.status.success());
    assert!(!fixture.directory.join("state").exists());
}

/// Operator-triggered read-only network probe, never part of offline acceptance.
/// Uses the existing service PATH/account wrappers; never supplies a new token.
#[tokio::test]
#[ignore = "requires explicit existing GitHub account and repository; performs real read-only API calls"]
async fn full_access_worker_reads_github_with_existing_service_account() {
    let repository = std::env::var("SUPERAGENT_LIVE_GH_REPOSITORY")
        .expect("explicit read-only repository required");
    let account =
        std::env::var("SUPERAGENT_LIVE_GH_ACCOUNT").expect("explicit existing account required");
    assert_eq!(repository.split('/').count(), 2);
    assert!(repository
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c)));
    let server = Server::with_responder(move |request| {
        if request["model"] != "worker" {
            return answer(request, false);
        }
        Ok(match calls(request).len() {
            0 => (
                Some("bash"),
                json!({"program":"python3","args":["-c",
                "import json,os,subprocess,sys; assert 'SUPERAGENT_FIXTURE_API_KEY' not in os.environ; run=lambda *a: subprocess.check_output(['gh',*a],timeout=30); user=json.loads(run('api','user')); assert user['login']==sys.argv[2]; issues=json.loads(run('api','repos/'+sys.argv[1]+'/issues?state=open&per_page=100')); assert isinstance(issues,list); print(json.dumps({'network':'allowed','account':user['login'],'openIssuesAndPulls':len(issues),'secretEnvScrubbed':True}))",repository,account]}),
            ),
            1 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Existing-account read-only GitHub network probe completed."}),
            ),
            _ => (None, Value::Null),
        })
    });
    let mut fixture = Fixture::new(&server.url);
    fixture.config["workerAccess"] = json!("danger-full-access");
    success(&fixture.run("--once").await);
    let requests = server.requests();
    let outputs: Vec<_> = requests
        .iter()
        .filter(|r| r["model"] == "worker")
        .flat_map(|r| r["messages"].as_array().unwrap())
        .filter(|m| m["role"] == "tool")
        .map(message_text)
        .collect();
    assert!(
        outputs
            .iter()
            .any(|s| s.contains("openIssuesAndPulls") && s.contains("secretEnvScrubbed")),
        "Actual Worker gh read failed: {outputs:?}"
    );
    for output in outputs.iter().filter(|s| s.contains("openIssuesAndPulls")) {
        println!("read-only Worker probe: {output}");
    }
}
