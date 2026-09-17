use crate::app::App;
use crate::events::AppEvent;
use crate::harness::transcript::{
    Message, Part, PartKind, Role, TokenUsage, ToolInfo, ToolStatus, TranscriptUpdate,
};
use crate::harness::HarnessEvent;
use crate::models::ModelRef;
use crate::panes::{Cell, PaneGrid, Row, Scheme};
use crate::session::{SessionState, SessStatus};
use std::path::PathBuf;
use std::time::Duration;

pub fn setup_demo_app(app: &mut App, theme: Option<&str>) {
    let t = theme.unwrap_or("ember-gruv");
    crate::theme::set_theme(t);
    app.is_demo = true;

    let sessions_meta = [
        (1, "auth", "src/auth.rs", "anthropic", "claude-3-5-sonnet"),
        (2, "auth-tests", "tests/auth_test.rs", "anthropic", "claude-3-opus"),
        (3, "auth-plan", "docs/plan.md", "anthropic", "claude-3-5-sonnet"),
        (4, "auth-dev", "server", "anthropic", "claude-3-5-sonnet"),
    ];

    for (id, name, path, provider, model) in sessions_meta {
        let dir = PathBuf::from(path);
        let mut sess = SessionState::new(id, name.to_string(), dir);
        sess.oc_sid = Some(format!("demo-{id}"));
        sess.status = SessStatus::Idle;
        sess.stick_bottom = true;
        sess.model = Some(ModelRef {
            provider_id: provider.to_string(),
            model_id: model.to_string(),
        });
        app.sessions.push(sess);
    }

    app.grid = PaneGrid {
        scheme: Scheme::Auto,
        rows: vec![
            Row {
                weight: 1.0,
                cells: vec![
                    Cell { weight: 1.0, session: 1 },
                    Cell { weight: 1.0, session: 2 },
                ],
            },
            Row {
                weight: 1.0,
                cells: vec![
                    Cell { weight: 1.0, session: 3 },
                    Cell { weight: 1.0, session: 4 },
                ],
            },
        ],
    };
    app.focus = 1;
    app.dirty = true;
}

fn user_msg(id: &str, text: &str) -> Message {
    Message {
        id: id.into(),
        role: Role::User,
        error: None,
        completed: Some(1710000000),
        created: Some(1710000000),
        cost: None,
        tokens: None,
        parts: vec![Part {
            id: format!("{id}-p0"),
            message_id: id.into(),
            kind: PartKind::Text {
                text: text.into(),
                synthetic: false,
            },
        }],
    }
}

fn asst_msg(id: &str, parts: Vec<Part>, cost: f64, tokens: TokenUsage) -> Message {
    Message {
        id: id.into(),
        role: Role::Assistant,
        error: None,
        completed: Some(1710000010),
        created: Some(1710000001),
        cost: Some(cost),
        tokens: Some(tokens),
        parts,
    }
}

fn text_part(msg_id: &str, part_id: &str, text: &str) -> Part {
    Part {
        id: part_id.into(),
        message_id: msg_id.into(),
        kind: PartKind::Text {
            text: text.into(),
            synthetic: false,
        },
    }
}

fn tool_part(msg_id: &str, part_id: &str, info: ToolInfo) -> Part {
    Part {
        id: part_id.into(),
        message_id: msg_id.into(),
        kind: PartKind::Tool(info),
    }
}

fn send_ev(
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
    oc_sid: &str,
    event: HarnessEvent,
) {
    let _ = tx.send(AppEvent::Harness {
        dir: PathBuf::new(),
        oc_sid: oc_sid.into(),
        event,
    });
}

fn send_transcript(
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
    oc_sid: &str,
    msgs: Vec<Message>,
) {
    send_ev(
        tx,
        oc_sid,
        HarnessEvent::Transcript(TranscriptUpdate::ReplaceAll(msgs)),
    );
}

pub async fn run_demo_loop(tx: tokio::sync::mpsc::UnboundedSender<AppEvent>) {
    let usage_base = TokenUsage {
        input: 3840,
        output: 480,
        reasoning: 920,
        cache_read: 14200,
        cache_write: 0,
    };

    loop {
        // --- STAGE 0: Initial seeding of all 4 panes ---
        let mut p1_msgs = vec![
            user_msg("a-u1", "Implement PKCE code challenge verification in src/auth.rs"),
        ];
        send_transcript(&tx, "demo-1", p1_msgs.clone());
        send_ev(&tx, "demo-1", HarnessEvent::SessionThinking);

        let mut p2_msgs = vec![
            user_msg("t-u1", "Run auth integration test suite"),
            asst_msg("t-a0", vec![
                text_part("t-a0", "t-p0", "Watching workspace for auth module modifications..."),
            ], 0.005, usage_base),
        ];
        send_transcript(&tx, "demo-2", p2_msgs.clone());
        send_ev(&tx, "demo-2", HarnessEvent::SessionIdle);

        let mut p3_items = vec![
            "• [✓] Step 1: Define OAuth2 token exchange & verification structs",
            "• [●] Step 2: Implement PKCE code challenge verifier (S256)",
            "• [○] Step 3: Implement unit and integration test suite",
            "• [○] Step 4: Validate dev server hot-reload & live callback",
        ];
        let mut p3_msgs = vec![
            user_msg("pl-u1", "Create implementation plan for OAuth2 PKCE auth flow"),
            asst_msg("pl-a1", vec![
                text_part("pl-a1", "pl-p1", &format!("Here is the architectural execution plan:\n\n{}", p3_items.join("\n"))),
            ], 0.010, usage_base),
        ];
        send_transcript(&tx, "demo-3", p3_msgs.clone());
        send_ev(&tx, "demo-3", HarnessEvent::SessionWorking);

        let mut dev_logs = vec![
            "ready - started server on 0.0.0.0:3000, url: http://localhost:3000".to_string(),
            "[GET] / 200 OK (1.4ms)".to_string(),
        ];
        let mut p4_msgs = vec![
            user_msg("d-u1", "Start development server and monitor auth routes"),
            asst_msg("d-a1", vec![
                text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
                tool_part("d-a1", "d-p2", ToolInfo {
                    tool: "bash".into(),
                    call_id: "c-dev-0".into(),
                    status: ToolStatus::Running,
                    title: Some("$ npm run dev".into()),
                    input: serde_json::json!({ "command": "npm run dev" }),
                    output: Some("compiling /api/auth... 22%".into()),
                    error: None,
                    metadata: serde_json::Value::Null,
                    start_ms: Some(1710000000),
                }),
            ], 0.008, usage_base),
        ];
        send_transcript(&tx, "demo-4", p4_msgs.clone());
        send_ev(&tx, "demo-4", HarnessEvent::SessionWorking);

        tokio::time::sleep(Duration::from_millis(400)).await;

        // --- STAGE 1: Agent 1 reads src/auth.rs while Dev advances ---
        send_ev(&tx, "demo-1", HarnessEvent::SessionWorking);
        p1_msgs.push(asst_msg("a-a1", vec![
            tool_part("a-a1", "a-p1", ToolInfo {
                tool: "read".into(),
                call_id: "c-a-read".into(),
                status: ToolStatus::Running,
                title: Some("Reading src/auth.rs".into()),
                input: serde_json::json!({ "filePath": "src/auth.rs" }),
                output: None,
                error: None,
                metadata: serde_json::json!({ "filePath": "src/auth.rs" }),
                start_ms: Some(1710000001),
            }),
        ], 0.012, usage_base));
        send_transcript(&tx, "demo-1", p1_msgs.clone());

        // Dev progress to 45%
        p4_msgs[1] = asst_msg("d-a1", vec![
            text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
            tool_part("d-a1", "d-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-dev-0".into(),
                status: ToolStatus::Running,
                title: Some("$ npm run dev".into()),
                input: serde_json::json!({ "command": "npm run dev" }),
                output: Some("compiling /api/auth... 45%".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000000),
            }),
        ], 0.009, usage_base);
        send_transcript(&tx, "demo-4", p4_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // Read completes, starts editing
        p1_msgs[1] = asst_msg("a-a1", vec![
            tool_part("a-a1", "a-p1", ToolInfo {
                tool: "read".into(),
                call_id: "c-a-read".into(),
                status: ToolStatus::Completed,
                title: Some("Reading src/auth.rs".into()),
                input: serde_json::json!({ "filePath": "src/auth.rs" }),
                output: Some("// src/auth.rs\npub struct PkceSession {\n    pub verifier: String,\n}".into()),
                error: None,
                metadata: serde_json::json!({ "filePath": "src/auth.rs" }),
                start_ms: Some(1710000001),
            }),
            tool_part("a-a1", "a-p2", ToolInfo {
                tool: "edit".into(),
                call_id: "c-a-edit".into(),
                status: ToolStatus::Running,
                title: Some("Editing src/auth.rs".into()),
                input: serde_json::json!({ "filePath": "src/auth.rs" }),
                output: None,
                error: None,
                metadata: serde_json::json!({ "filePath": "src/auth.rs" }),
                start_ms: Some(1710000002),
            }),
        ], 0.019, usage_base);
        send_transcript(&tx, "demo-1", p1_msgs.clone());

        // Dev progress to 75%
        p4_msgs[1] = asst_msg("d-a1", vec![
            text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
            tool_part("d-a1", "d-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-dev-0".into(),
                status: ToolStatus::Running,
                title: Some("$ npm run dev".into()),
                input: serde_json::json!({ "command": "npm run dev" }),
                output: Some("compiling /api/auth... 75%".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000000),
            }),
        ], 0.010, usage_base);
        send_transcript(&tx, "demo-4", p4_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // Edit completes with diff!
        let diff_1 = "@@ -42,6 +42,14 @@ pub fn verify_pkce_challenge(\n     let mut hasher = Sha256::new();\n     hasher.update(verifier.as_bytes());\n+    let hash = hasher.finalize();\n+    let computed = URL_SAFE_NO_PAD.encode(hash);\n+    // Constant time comparison to prevent timing attacks\n+    subtle::ConstantTimeEq::ct_eq(\n+        computed.as_bytes(),\n+        challenge.as_bytes(),\n+    ).into()\n }\n";
        p1_msgs[1] = asst_msg("a-a1", vec![
            tool_part("a-a1", "a-p1", ToolInfo {
                tool: "read".into(),
                call_id: "c-a-read".into(),
                status: ToolStatus::Completed,
                title: Some("Reading src/auth.rs".into()),
                input: serde_json::json!({ "filePath": "src/auth.rs" }),
                output: Some("File read successfully".into()),
                error: None,
                metadata: serde_json::json!({ "filePath": "src/auth.rs" }),
                start_ms: Some(1710000001),
            }),
            tool_part("a-a1", "a-p2", ToolInfo {
                tool: "edit".into(),
                call_id: "c-a-edit".into(),
                status: ToolStatus::Completed,
                title: Some("Editing src/auth.rs".into()),
                input: serde_json::json!({ "filePath": "src/auth.rs" }),
                output: Some("Updated src/auth.rs (+8 lines)".into()),
                error: None,
                metadata: serde_json::json!({ "filePath": "src/auth.rs", "diff": diff_1 }),
                start_ms: Some(1710000002),
            }),
            text_part("a-a1", "a-p3", "Implemented constant-time `verify_pkce_challenge` with SHA-256."),
        ], 0.038, TokenUsage {
            input: 4210,
            output: 640,
            reasoning: 1100,
            cache_read: 15100,
            cache_write: 0,
        });
        send_transcript(&tx, "demo-1", p1_msgs.clone());
        send_ev(&tx, "demo-1", HarnessEvent::SessionIdle);

        // Plan updates: Step 2 checked!
        p3_items[1] = "• [✓] Step 2: Implement PKCE code challenge verifier (S256)";
        p3_items[2] = "• [●] Step 3: Implement unit and integration test suite";
        p3_msgs[1] = asst_msg("pl-a1", vec![
            text_part("pl-a1", "pl-p1", &format!("Here is the architectural execution plan:\n\n{}", p3_items.join("\n"))),
        ], 0.015, usage_base);
        send_transcript(&tx, "demo-3", p3_msgs.clone());

        // Dev server compiled 100%, now ready and serves request!
        dev_logs.push("[GET] /api/auth/csrf 200 OK (token generated in 0.8ms)".into());
        p4_msgs[1] = asst_msg("d-a1", vec![
            text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
            tool_part("d-a1", "d-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-dev-0".into(),
                status: ToolStatus::Completed,
                title: Some("$ npm run dev".into()),
                input: serde_json::json!({ "command": "npm run dev" }),
                output: Some(dev_logs.join("\n")),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000000),
            }),
        ], 0.012, usage_base);
        send_transcript(&tx, "demo-4", p4_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // --- STAGE 2: Agent 2 runs integration tests with progress bar ---
        send_ev(&tx, "demo-2", HarnessEvent::SessionWorking);
        p2_msgs.push(asst_msg("t-a1", vec![
            text_part("t-a1", "t-p1", "Detected changes in src/auth.rs. Running integration test suite:"),
            tool_part("t-a1", "t-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-t-1".into(),
                status: ToolStatus::Running,
                title: Some("$ cargo test --test auth_integration".into()),
                input: serde_json::json!({ "command": "cargo test --test auth_integration" }),
                output: Some("Building test binary... 38%".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000005),
            }),
        ], 0.014, usage_base));
        send_transcript(&tx, "demo-2", p2_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // Test progress to 72%
        p2_msgs[1] = asst_msg("t-a1", vec![
            text_part("t-a1", "t-p1", "Detected changes in src/auth.rs. Running integration test suite:"),
            tool_part("t-a1", "t-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-t-1".into(),
                status: ToolStatus::Running,
                title: Some("$ cargo test --test auth_integration".into()),
                input: serde_json::json!({ "command": "cargo test --test auth_integration" }),
                output: Some("Running 4 tests... 72%".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000005),
            }),
        ], 0.018, usage_base);
        send_transcript(&tx, "demo-2", p2_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // Tests complete (4 passed!)
        p2_msgs[1] = asst_msg("t-a1", vec![
            text_part("t-a1", "t-p1", "Detected changes in src/auth.rs. Running integration test suite:"),
            tool_part("t-a1", "t-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-t-1".into(),
                status: ToolStatus::Completed,
                title: Some("$ cargo test --test auth_integration".into()),
                input: serde_json::json!({ "command": "cargo test --test auth_integration" }),
                output: Some("running 4 tests\ntest tests::test_code_verifier_validation ... ok\ntest tests::test_sha256_pkce_challenge_match ... ok\ntest tests::test_invalid_challenge_rejected ... ok\ntest tests::test_token_exchange_success ... ok\n\ntest result: ok. 4 passed; 0 failed; finished in 0.18s".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000005),
            }),
            text_part("t-a1", "t-p3", "All 4 integration tests passed ✓ (0 failures, 180ms)."),
        ], 0.026, TokenUsage {
            input: 4100,
            output: 512,
            reasoning: 890,
            cache_read: 14800,
            cache_write: 0,
        });
        send_transcript(&tx, "demo-2", p2_msgs.clone());
        send_ev(&tx, "demo-2", HarnessEvent::SessionIdle);

        // Plan marks Step 3 complete, Step 4 in progress
        p3_items[2] = "• [✓] Step 3: Implement unit and integration test suite (4 passed)";
        p3_items[3] = "• [●] Step 4: Validate dev server hot-reload & live callback";
        p3_msgs[1] = asst_msg("pl-a1", vec![
            text_part("pl-a1", "pl-p1", &format!("Here is the architectural execution plan:\n\n{}", p3_items.join("\n"))),
        ], 0.018, usage_base);
        send_transcript(&tx, "demo-3", p3_msgs.clone());

        // Dev server logs live PKCE token exchange
        dev_logs.push("[POST] /api/auth/token 200 OK (PKCE verified in 9.2ms)".into());
        dev_logs.push("[GET] /api/auth/session 200 OK (session cookie issued)".into());
        p4_msgs[1] = asst_msg("d-a1", vec![
            text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
            tool_part("d-a1", "d-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-dev-0".into(),
                status: ToolStatus::Completed,
                title: Some("$ npm run dev".into()),
                input: serde_json::json!({ "command": "npm run dev" }),
                output: Some(dev_logs.join("\n")),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000000),
            }),
            text_part("d-a1", "d-p3", "Live auth callback verified successfully."),
        ], 0.015, usage_base);
        send_transcript(&tx, "demo-4", p4_msgs.clone());

        // Plan marks Step 4 complete!
        p3_items[3] = "• [✓] Step 4: Validate dev server hot-reload & live callback";
        p3_items.push("• [●] Step 5: Implement Redis token revocation blacklist");
        p3_msgs[1] = asst_msg("pl-a1", vec![
            text_part("pl-a1", "pl-p1", &format!("Here is the architectural execution plan:\n\n{}", p3_items.join("\n"))),
        ], 0.021, usage_base);
        send_transcript(&tx, "demo-3", p3_msgs.clone());

        tokio::time::sleep(Duration::from_millis(500)).await;

        // --- STAGE 3: Turn 2 in Agent 1 (Scrolls down!) ---
        // New user prompt appended to Pane 1!
        p1_msgs.push(user_msg("a-u2", "Add Redis token revocation blacklist check in validate_token"));
        send_transcript(&tx, "demo-1", p1_msgs.clone());
        send_ev(&tx, "demo-1", HarnessEvent::SessionThinking);

        tokio::time::sleep(Duration::from_millis(450)).await;

        send_ev(&tx, "demo-1", HarnessEvent::SessionWorking);
        let diff_2 = "@@ -88,4 +88,10 @@ pub async fn validate_token(\n+    let key = format!(\"revoked:{}\", token);\n+    if redis.exists(&key).await? {\n+        return Err(AuthError::RevokedToken);\n+    }\n";
        p1_msgs.push(asst_msg("a-a2", vec![
            tool_part("a-a2", "a-p2-1", ToolInfo {
                tool: "edit".into(),
                call_id: "c-a-edit2".into(),
                status: ToolStatus::Completed,
                title: Some("Editing src/auth.rs".into()),
                input: serde_json::json!({ "filePath": "src/auth.rs" }),
                output: Some("Updated src/auth.rs (+6 lines)".into()),
                error: None,
                metadata: serde_json::json!({ "filePath": "src/auth.rs", "diff": diff_2 }),
                start_ms: Some(1710000010),
            }),
            text_part("a-a2", "a-p2-2", "Wired Redis blacklist check with non-blocking async lookup."),
        ], 0.054, TokenUsage {
            input: 5400,
            output: 820,
            reasoning: 1250,
            cache_read: 16800,
            cache_write: 0,
        }));
        send_transcript(&tx, "demo-1", p1_msgs.clone());
        send_ev(&tx, "demo-1", HarnessEvent::SessionIdle);

        // Dev server logs token revocation test
        dev_logs.push("[POST] /api/auth/revoke 200 OK (revoked in redis, ttl 86400s)".into());
        dev_logs.push("[GET] /api/auth/validate 401 Unauthorized (token revoked)".into());
        p4_msgs[1] = asst_msg("d-a1", vec![
            text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
            tool_part("d-a1", "d-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-dev-0".into(),
                status: ToolStatus::Completed,
                title: Some("$ npm run dev".into()),
                input: serde_json::json!({ "command": "npm run dev" }),
                output: Some(dev_logs.join("\n")),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000000),
            }),
            text_part("d-a1", "d-p3", "Revocation endpoint verified live."),
        ], 0.018, usage_base);
        send_transcript(&tx, "demo-4", p4_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // --- STAGE 4: Pane 2 runs token revocation tests (Scrolls down!) ---
        send_ev(&tx, "demo-2", HarnessEvent::SessionWorking);
        p2_msgs.push(user_msg("t-u2", "Run token revocation unit tests"));
        p2_msgs.push(asst_msg("t-a2", vec![
            tool_part("t-a2", "t-p2-1", ToolInfo {
                tool: "bash".into(),
                call_id: "c-t-2".into(),
                status: ToolStatus::Running,
                title: Some("$ cargo test --test revocation_test".into()),
                input: serde_json::json!({ "command": "cargo test --test revocation_test" }),
                output: Some("Running 3 tests... 60%".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000015),
            }),
        ], 0.015, usage_base));
        send_transcript(&tx, "demo-2", p2_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        p2_msgs[3] = asst_msg("t-a2", vec![
            tool_part("t-a2", "t-p2-1", ToolInfo {
                tool: "bash".into(),
                call_id: "c-t-2".into(),
                status: ToolStatus::Completed,
                title: Some("$ cargo test --test revocation_test".into()),
                input: serde_json::json!({ "command": "cargo test --test revocation_test" }),
                output: Some("running 3 tests\ntest tests::test_redis_blacklist_lookup ... ok\ntest tests::test_revoked_token_rejected ... ok\ntest tests::test_active_token_allowed ... ok\n\ntest result: ok. 3 passed; 0 failed; finished in 0.12s".into()),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000015),
            }),
            text_part("t-a2", "t-p2-2", "All 3 revocation tests passed ✓ (0.12s)."),
        ], 0.028, TokenUsage {
            input: 4600,
            output: 580,
            reasoning: 950,
            cache_read: 15400,
            cache_write: 0,
        });
        send_transcript(&tx, "demo-2", p2_msgs.clone());
        send_ev(&tx, "demo-2", HarnessEvent::SessionIdle);

        // Plan updates: Step 5 checked!
        p3_items[4] = "• [✓] Step 5: Implement Redis token revocation blacklist";
        p3_items.push("• [●] Step 6: Deploy rate limiter with sliding window counter");
        p3_msgs[1] = asst_msg("pl-a1", vec![
            text_part("pl-a1", "pl-p1", &format!("Here is the architectural execution plan:\n\n{}", p3_items.join("\n"))),
        ], 0.024, usage_base);
        send_transcript(&tx, "demo-3", p3_msgs.clone());

        tokio::time::sleep(Duration::from_millis(450)).await;

        // --- STAGE 5: Turn 3 in Agent 1 (Scrolls further down!) ---
        p1_msgs.push(user_msg("a-u3", "Add rate limiter middleware in src/limiter.rs"));
        send_transcript(&tx, "demo-1", p1_msgs.clone());
        send_ev(&tx, "demo-1", HarnessEvent::SessionThinking);

        tokio::time::sleep(Duration::from_millis(450)).await;

        send_ev(&tx, "demo-1", HarnessEvent::SessionWorking);
        let diff_3 = "@@ -0,0 +1,14 @@\n+pub struct SlidingWindowLimiter {\n+    pub max_requests: usize,\n+    pub window_secs: u64,\n+}\n+impl SlidingWindowLimiter {\n+    pub async fn check(&self, ip: &str) -> bool {\n+        redis.incr_window(ip, self.window_secs).await < self.max_requests\n+    }\n+}\n";
        p1_msgs.push(asst_msg("a-a3", vec![
            tool_part("a-a3", "a-p3-1", ToolInfo {
                tool: "edit".into(),
                call_id: "c-a-edit3".into(),
                status: ToolStatus::Completed,
                title: Some("Editing src/limiter.rs".into()),
                input: serde_json::json!({ "filePath": "src/limiter.rs" }),
                output: Some("Created src/limiter.rs (+14 lines)".into()),
                error: None,
                metadata: serde_json::json!({ "filePath": "src/limiter.rs", "diff": diff_3 }),
                start_ms: Some(1710000020),
            }),
            text_part("a-a3", "a-p3-2", "Sliding window rate limiter configured (100 req/min)."),
        ], 0.068, TokenUsage {
            input: 6200,
            output: 980,
            reasoning: 1400,
            cache_read: 18200,
            cache_write: 0,
        }));
        send_transcript(&tx, "demo-1", p1_msgs.clone());
        send_ev(&tx, "demo-1", HarnessEvent::SessionIdle);

        // Dev server logs rate limit test
        dev_logs.push("[POST] /api/auth/token 429 Too Many Requests (rate limit exceeded: 100/min)".into());
        dev_logs.push("[GET] /api/health 200 OK (all services healthy)".into());
        p4_msgs[1] = asst_msg("d-a1", vec![
            text_part("d-a1", "d-p1", "Next.js dev server running on http://localhost:3000:"),
            tool_part("d-a1", "d-p2", ToolInfo {
                tool: "bash".into(),
                call_id: "c-dev-0".into(),
                status: ToolStatus::Completed,
                title: Some("$ npm run dev".into()),
                input: serde_json::json!({ "command": "npm run dev" }),
                output: Some(dev_logs.join("\n")),
                error: None,
                metadata: serde_json::Value::Null,
                start_ms: Some(1710000000),
            }),
            text_part("d-a1", "d-p3", "Rate limiter verified with 429 payload."),
        ], 0.022, usage_base);
        send_transcript(&tx, "demo-4", p4_msgs.clone());

        // Plan marks Step 6 complete!
        p3_items[5] = "• [✓] Step 6: Deploy rate limiter with sliding window counter";
        p3_items.push("• [✓] All architectural milestones completed & tested.");
        p3_msgs[1] = asst_msg("pl-a1", vec![
            text_part("pl-a1", "pl-p1", &format!("Here is the architectural execution plan:\n\n{}", p3_items.join("\n"))),
        ], 0.028, usage_base);
        send_transcript(&tx, "demo-3", p3_msgs.clone());
        send_ev(&tx, "demo-3", HarnessEvent::SessionIdle);

        // Hold completed state for 4 seconds so viewer appreciates the full multi-turn completion
        tokio::time::sleep(Duration::from_millis(4000)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::manager::Manager;

    #[test]
    fn test_setup_demo_app_creates_four_sessions_and_grid() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = Config::default();
        let manager = Manager::new(tx, cfg.clone());
        let mut app = App::new(cfg, manager, PathBuf::from("."));

        setup_demo_app(&mut app, None);

        assert_eq!(app.sessions.len(), 4);
        assert_eq!(app.sessions[0].name, "auth");
        assert_eq!(app.sessions[1].name, "auth-tests");
        assert_eq!(app.sessions[2].name, "auth-plan");
        assert_eq!(app.sessions[3].name, "auth-dev");

        assert_eq!(app.sessions[0].oc_sid.as_deref(), Some("demo-1"));
        assert_eq!(app.sessions[1].oc_sid.as_deref(), Some("demo-2"));
        assert_eq!(app.sessions[2].oc_sid.as_deref(), Some("demo-3"));
        assert_eq!(app.sessions[3].oc_sid.as_deref(), Some("demo-4"));

        assert_eq!(app.grid.rows.len(), 2);
        assert_eq!(app.grid.rows[0].cells.len(), 2);
        assert_eq!(app.grid.rows[1].cells.len(), 2);
        assert_eq!(app.focus, 1);
    }
}
