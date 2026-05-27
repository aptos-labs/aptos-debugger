use aptos_dap::server::{variables::frame_locals_ref_id, DapServer, RunCommand};
use indexmap::IndexMap;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::PathBuf,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

fn sanitize_frame(frame: &serde_json::Value) -> serde_json::Value {
    let mut f = frame.clone();
    if let Some(path) = f["source"]["path"].as_str() {
        let re = regex::Regex::new(r"/tmp/dap_tests/[^/]+/").unwrap();
        f["source"]["path"] = serde_json::Value::String(re.replace(path, "$$TMPDIR/").to_string());
    }
    f
}

fn parse_breakpoints(bps: &[&str]) -> BTreeMap<String, Vec<i64>> {
    let mut by_file: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for bp in bps {
        let (path, line_str) = bp
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("invalid breakpoint format '{bp}', expected 'path:line'"));
        let line: i64 = line_str.parse().unwrap();
        by_file.entry(path.to_string()).or_default().push(line);
    }
    by_file
}

pub const RECV_TIMEOUT: Duration = Duration::from_secs(3);

pub struct DapTestServer {
    msg_rx: mpsc::Receiver<serde_json::Value>,
    writer: Option<Box<dyn Write>>,
    seq: u64,
    server_thread: Option<JoinHandle<()>>,
    _reader_thread: JoinHandle<()>,
}

pub struct TestPackage {
    _tmp: tempfile::TempDir,
    pub path: PathBuf,
    pub breakpoints: BTreeMap<String, String>,
}

pub fn test_mode(pkg: &TestPackage) -> RunCommand {
    RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    }
}

impl TestPackage {
    pub fn all_bps(&self) -> Vec<&str> {
        self.breakpoints.values().map(|s| s.as_str()).collect()
    }
}

pub fn build_test_package(source: &str) -> TestPackage {
    let test_name = thread::current().name().unwrap_or("unknown").to_string();
    let base = PathBuf::from("/tmp/dap_tests");
    std::fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::Builder::new()
        .prefix(&format!("{test_name}_"))
        .tempdir_in(&base)
        .unwrap();
    let pkg = tmp.path().to_path_buf();
    let sources = pkg.join("sources");
    std::fs::create_dir_all(&sources).unwrap();
    std::fs::write(
        pkg.join("Move.toml"),
        "[package]\nname = \"Test\"\nversion = \"0.0.1\"\n",
    )
    .unwrap();
    let source_path = sources.join("test.move");
    std::fs::write(&source_path, &source).unwrap();
    let bp_re = regex::Regex::new(r"//\s*bp(\d+)").unwrap();
    let breakpoints = source
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            bp_re.captures(l).map(|caps| {
                let name = format!("bp{}", &caps[1]);
                let loc = format!("{}:{}", source_path.to_string_lossy(), i + 1);
                (name, loc)
            })
        })
        .collect();
    TestPackage {
        _tmp: tmp,
        path: pkg,
        breakpoints,
    }
}

impl DapTestServer {
    pub fn start(mode: RunCommand) -> Self {
        let (server_reader, client_writer) = std::io::pipe().unwrap();
        let (client_reader, server_writer) = std::io::pipe().unwrap();

        let server_thread = thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let server_input = BufReader::new(server_reader);
                let server_output = BufWriter::new(server_writer);
                let mut server = DapServer::new(server_input, server_output, mode).unwrap();
                let _ = server.run();
            })
            .unwrap();

        let (msg_tx, msg_rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut reader = BufReader::new(client_reader);
            loop {
                let len: usize = loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 {
                        return;
                    }
                    let trimmed = header.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                        break rest.trim().parse().unwrap();
                    }
                };
                let mut blank = String::new();
                if reader.read_line(&mut blank).unwrap_or(0) == 0 {
                    return;
                }
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                let msg: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if msg_tx.send(msg).is_err() {
                    return;
                }
            }
        });

        Self {
            msg_rx,
            writer: Some(Box::new(client_writer)),
            seq: 1,
            server_thread: Some(server_thread),
            _reader_thread: reader_thread,
        }
    }

    pub fn send(&mut self, command: &str, args: Option<serde_json::Value>) {
        let mut payload = serde_json::json!({
            "seq": self.seq,
            "type": "request",
            "command": command,
        });
        if let Some(a) = args {
            payload["arguments"] = a;
        }
        self.seq += 1;
        let s = serde_json::to_string(&payload).unwrap();
        let msg = format!("Content-Length: {}\r\n\r\n{}", s.len(), s);
        let writer = self.writer.as_mut().expect("server already disconnected");
        writer.write_all(msg.as_bytes()).unwrap();
        writer.flush().unwrap();
    }

    fn recv_with_timeout(&mut self, timeout: Duration) -> serde_json::Value {
        self.msg_rx
            .recv_timeout(timeout)
            .expect("timed out waiting for DAP message")
    }

    pub fn collect_until_response(&mut self, cmd: &str, max: usize) -> serde_json::Value {
        self.collect_until_response_timeout(cmd, max, RECV_TIMEOUT)
    }

    pub fn collect_until_response_timeout(
        &mut self,
        cmd: &str,
        max: usize,
        timeout: Duration,
    ) -> serde_json::Value {
        for _ in 0..max {
            let m = self.recv_with_timeout(timeout);
            if m["type"] == "response" && m["command"] == cmd {
                return m;
            }
        }
        panic!("no response for '{cmd}' within {max} messages");
    }

    pub fn collect_until_event(&mut self, event: &str, max: usize) -> serde_json::Value {
        self.collect_until_event_timeout(event, max, RECV_TIMEOUT)
    }

    pub fn collect_until_event_timeout(
        &mut self,
        event: &str,
        max: usize,
        timeout: Duration,
    ) -> serde_json::Value {
        for _ in 0..max {
            let m = self.recv_with_timeout(timeout);
            if m["type"] == "event" && m["event"] == event {
                return m;
            }
        }
        panic!("no '{event}' event within {max} messages");
    }

    pub fn collect_until_event_any(
        &mut self,
        events: &[&str],
        timeout: Duration,
    ) -> serde_json::Value {
        for _ in 0..30 {
            let m = self.recv_with_timeout(timeout);
            if m["type"] == "event" {
                if let Some(evt) = m["event"].as_str() {
                    if events.contains(&evt) {
                        return m;
                    }
                }
            }
        }
        panic!("no event matching {events:?} within 30 messages");
    }

    pub fn initialize_and_launch_test(&mut self, pkg: &TestPackage) {
        self.initialize();
        self.launch();

        self.set_breakpoints(&pkg.all_bps());

        self.send("configurationDone", None);
        self.collect_until_event("stopped", 30);
    }

    pub fn initialize_and_launch_replay(&mut self, breakpoints: &[&str], timeout: Duration) {
        self.initialize();
        self.launch();

        self.set_breakpoints(breakpoints);

        self.send("configurationDone", None);
        self.collect_until_event_timeout("stopped", 30, timeout);
    }

    pub fn initialize(&mut self) {
        self.send(
            "initialize",
            Some(serde_json::json!({
                "adapterID": "test",
                "linesStartAt1": true,
                "columnsStartAt1": true,
            })),
        );
        self.collect_until_event("initialized", 10);
    }

    pub fn launch(&mut self) {
        self.send("launch", Some(serde_json::json!({})));
        self.collect_until_response("launch", 10);
    }

    pub fn set_breakpoints(&mut self, breakpoints: &[&str]) {
        let by_file = parse_breakpoints(breakpoints);
        for (path, lines) in &by_file {
            let bps: Vec<_> = lines
                .iter()
                .map(|l| serde_json::json!({ "line": l }))
                .collect();
            self.send(
                "setBreakpoints",
                Some(serde_json::json!({
                    "source": { "path": path },
                    "breakpoints": bps,
                })),
            );
            self.collect_until_response("setBreakpoints", 10);
        }
    }

    pub fn assert_frame_scopes(&mut self, frame_id: usize, expected: expect_test::Expect) {
        let scopes = self.get_frame_scopes(frame_id);
        let arr: Vec<_> = scopes.values().collect();
        expected.assert_eq(&serde_json::to_string_pretty(&arr).unwrap());
    }

    pub fn get_frame_scopes(&mut self, frame_id: usize) -> IndexMap<String, serde_json::Value> {
        self.send("scopes", Some(serde_json::json!({ "frameId": frame_id })));
        let resp = self.collect_until_response("scopes", 5);
        resp["body"]["scopes"]
            .as_array()
            .expect("no scopes array")
            .iter()
            .map(|s| {
                let name = s["name"].as_str().unwrap().to_string();
                (name, s.clone())
            })
            .collect()
    }

    pub fn get_stack_frames(&mut self) -> Vec<serde_json::Value> {
        self.send("stackTrace", Some(serde_json::json!({ "threadId": 1 })));
        let resp = self.collect_until_response("stackTrace", 5);
        resp["body"]["stackFrames"]
            .as_array()
            .expect("no stackFrames")
            .clone()
    }

    pub fn assert_stack_frames(&mut self, expected: expect_test::Expect) {
        let frames = self.get_stack_frames();
        let sanitized: Vec<_> = frames.iter().map(|f| sanitize_frame(f)).collect();
        expected.assert_eq(&serde_json::to_string_pretty(&sanitized).unwrap());
    }

    pub fn assert_frame_variables(&mut self, frame_id: usize, expected: expect_test::Expect) {
        self.assert_variables(frame_locals_ref_id(frame_id as i64), expected);
    }

    pub fn assert_variables(&mut self, variable_ref_id: i64, expected: expect_test::Expect) {
        let vars = self.get_variables_by_reference(variable_ref_id);
        let arr: Vec<_> = vars.values().collect();
        expected.assert_eq(&serde_json::to_string_pretty(&arr).unwrap());
    }

    pub fn assert_variables_timeout(
        &mut self,
        variable_ref_id: i64,
        timeout: Duration,
        expected: expect_test::Expect,
    ) {
        let vars = self.get_variables_by_reference_timeout(variable_ref_id, timeout);
        let arr: Vec<_> = vars.values().collect();
        expected.assert_eq(&serde_json::to_string_pretty(&arr).unwrap());
    }

    pub fn get_variables_by_reference(
        &mut self,
        variable_ref_id: i64,
    ) -> IndexMap<String, serde_json::Value> {
        self.get_variables_by_reference_timeout(variable_ref_id, RECV_TIMEOUT)
    }

    pub fn get_variables_by_reference_timeout(
        &mut self,
        variable_ref_id: i64,
        timeout: Duration,
    ) -> IndexMap<String, serde_json::Value> {
        self.send(
            "variables",
            Some(serde_json::json!({ "variablesReference": variable_ref_id })),
        );
        let resp = self.collect_until_response_timeout("variables", 5, timeout);
        resp["body"]["variables"]
            .as_array()
            .expect("no variables array")
            .iter()
            .map(|v| {
                let name = v["name"].as_str().unwrap().to_string();
                (name, v.clone())
            })
            .collect()
    }

    pub fn continue_until_breakpoint(&mut self) {
        self.send("continue", Some(serde_json::json!({ "threadId": 1 })));
        let evt = self.collect_until_event("stopped", 30);
        assert_eq!(evt["body"]["reason"].as_str(), Some("breakpoint"));
    }

    pub fn step_over(&mut self) {
        self.send("next", Some(serde_json::json!({ "threadId": 1 })));
        self.collect_until_event("stopped", 30);
    }

    pub fn continue_execution_timeout(&mut self, timeout: Duration) -> bool {
        self.send("continue", Some(serde_json::json!({ "threadId": 1 })));
        for _ in 0..30 {
            let m = self.recv_with_timeout(timeout);
            if m["type"] == "event" {
                match m["event"].as_str() {
                    Some("stopped") => return true,
                    Some("terminated") => return false,
                    _ => {}
                }
            }
        }
        panic!("no stopped/terminated event within 30 messages");
    }
}

impl Drop for DapTestServer {
    fn drop(&mut self) {
        self.send("disconnect", None);
        drop(self.writer.take());
        if let Some(t) = self.server_thread.take() {
            let _ = t.join();
        }
    }
}
