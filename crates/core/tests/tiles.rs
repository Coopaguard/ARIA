//! The tile sandbox, on a real socket.
//!
//! A unit test of the registry cannot see the wire, and the wire is where the
//! two things that matter live: the **Content-Security-Policy header**, which
//! is what binds HTML written by a language model, and the absence of
//! `Access-Control-Allow-Origin`, which is what keeps the window's origin out
//! of reach. Both are headers. Neither appears in a `HashMap` assertion.
//!
//! Spoken in raw HTTP/1.1 rather than through a client crate, deliberately:
//! the subject of the test *is* the bytes of the response, and a client that
//! parses them away would hide the one thing being checked.

use std::sync::Arc;

use aria_core::sessions::SessionId;
use aria_core::tiles::Registry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Start the sandbox on a port the OS picks, and give back where it listens.
async fn sandbox(tiles: Arc<Registry>) -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
    let port = listener.local_addr().expect("an address").port();
    tokio::spawn(async move {
        let _ = aria_core::tiles::serve_on(listener, tiles).await;
    });
    port
}

/// One request, one response, as text.
async fn get(port: u16, path: &str) -> String {
    get_with(port, path, "").await
}

/// The same, with extra request headers.
async fn get_with(port: u16, path: &str, extra: &str) -> String {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the sandbox is listening");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}Connection: close\r\n\r\n"
    );
    socket
        .write_all(request.as_bytes())
        .await
        .expect("the request goes out");
    let mut answer = Vec::new();
    socket
        .read_to_end(&mut answer)
        .await
        .expect("the answer comes back");
    String::from_utf8_lossy(&answer).to_string()
}

#[tokio::test]
async fn a_tile_is_served_with_the_policy_that_binds_it() {
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(
        SessionId::new(),
        "Météo",
        "<!doctype html><html><body><p>23 degrés</body></html>",
        Vec::new(),
        None,
    );
    let port = sandbox(Arc::clone(&tiles)).await;

    let answer = get(port, &tile.path()).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(
        answer.contains("23 degrés"),
        "the document is served as it is"
    );

    // The floor of SEP-1865, on the response and not only in a meta tag the
    // model may have forgotten to write.
    let policy = answer
        .lines()
        .find(|line| {
            line.to_ascii_lowercase()
                .starts_with("content-security-policy:")
        })
        .expect("a CSP header");
    for clause in [
        "default-src 'none'",
        // Celle qui compte le plus : une tuile ne peut rien envoyer ailleurs.
        "connect-src 'none'",
        // Tout ce qu'elle charge vient de cette origine, donc de fichiers
        // qu'une session a déclarés — jamais d'un hôte distant.
        "frame-src 'self'",
        "media-src 'self'",
    ] {
        assert!(policy.contains(clause), "manque {clause} dans « {policy} »");
    }

    // A tile talks to the host over postMessage. Nothing is fetched
    // cross-origin, so nothing is allowed to be.
    assert!(
        !answer
            .to_ascii_lowercase()
            .contains("access-control-allow-origin"),
        "the sandbox must not hand the window's origin a reader:\n{answer}"
    );
}

/// A tile the registry never minted, or one the window has closed.
#[tokio::test]
async fn a_tile_that_does_not_exist_is_a_404_and_not_an_empty_page() {
    let port = sandbox(Arc::new(Registry::default())).await;
    let answer = get(port, "/t/1").await;
    assert!(answer.starts_with("HTTP/1.1 404"), "{answer}");
}

/// Closing a tile has to make it unreachable, not merely invisible: the window
/// is not the only thing that can hold the URL.
#[tokio::test]
async fn a_closed_tile_stops_being_served() {
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(
        SessionId::new(),
        "Météo",
        "<!doctype html><p>23",
        Vec::new(),
        None,
    );
    let port = sandbox(Arc::clone(&tiles)).await;

    assert!(get(port, &tile.path()).await.starts_with("HTTP/1.1 200"));
    tiles.remove(&tile.uri);
    assert!(get(port, &tile.path()).await.starts_with("HTTP/1.1 404"));
}

#[tokio::test]
async fn the_origin_answers_before_any_tile_exists() {
    let port = sandbox(Arc::new(Registry::default())).await;
    let answer = get(port, "/health").await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.trim_end().ends_with("ok"), "{answer}");
}

/// The two halves together: the core registers a tile, says so on the stream,
/// and the sandbox then serves exactly what the event named.
///
/// Separately from the registry's own tests because this is the join — a tile
/// held but never announced, or announced under a URI that is not served, are
/// both invisible on screen and both pass a unit test.
#[tokio::test]
async fn a_tile_is_announced_under_the_uri_that_is_then_served() {
    let core = aria_core::Core::new();
    let mut events = core.broker().subscribe();
    let port = sandbox(Arc::clone(core.tiles())).await;

    let uri = core.show_tile(
        SessionId::new(),
        "Météo",
        "<!doctype html><html><body><p>23 degrés</body></html>",
        Vec::new(),
        None,
    );

    let announced = loop {
        let envelope = events.recv().await.expect("the stream stays open");
        if let aria_core::events::Event::TileReady { uri, title, .. } = envelope.event {
            break (uri, title);
        }
    };
    assert_eq!(announced, (uri.clone(), "Météo".to_string()));

    let tile = core.tiles().get(&uri).expect("the core holds it");
    let answer = get(port, &tile.path()).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains("23 degrés"));
}

/// The form and the PDFs ride on the event, and a PDF's URL is served as one.
///
/// Both were found on 2026-11-16. The window read `form` off `tile.ready` and
/// the core never put it there, so no declared form ever reached the screen;
/// and a PDF inside a tile is blank in Chromium because the tile is a
/// sandboxed frame, so the window frames it beside the tile instead.
#[tokio::test]
async fn the_event_carries_the_form_and_the_documents_the_window_draws() {
    let core = aria_core::Core::new();
    let mut events = core.broker().subscribe();
    let port = sandbox(Arc::clone(core.tiles())).await;

    let dir = std::env::temp_dir().join(format!("aria-pdf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("lettre.pdf");
    std::fs::write(&pdf, b"%PDF-1.4 test").unwrap();
    let song = dir.join("chanson.flac");
    std::fs::write(&song, b"fLaC").unwrap();

    let form = aria_core::tiles::Form {
        submit: "Envoyer".into(),
        fields: vec![aria_core::tiles::Field {
            name: "objet".into(),
            label: "Objet".into(),
            kind: aria_core::tiles::FieldKind::Text,
            options: Vec::new(),
            value: String::new(),
        }],
    };
    core.show_tile(
        SessionId::new(),
        "Courrier",
        "<p>brouillon</p>",
        vec![
            aria_core::tiles::TileFile { name: "lettre.pdf".into(), path: pdf.display().to_string() },
            aria_core::tiles::TileFile { name: "chanson.flac".into(), path: song.display().to_string() },
        ],
        Some(form.clone()),
    );

    let (carried, documents) = loop {
        let envelope = events.recv().await.expect("the stream stays open");
        if let aria_core::events::Event::TileReady { form, documents, .. } = envelope.event {
            break (form, documents);
        }
    };
    assert_eq!(carried, Some(form));
    // The PDF and only the PDF: audio plays inside the tile.
    assert_eq!(documents.len(), 1, "{documents:?}");
    assert!(documents[0].ends_with("/f/lettre.pdf"), "{}", documents[0]);

    // And the URL the window will frame answers as a PDF that cannot be
    // sniffed into anything else — which is what makes it safe unsandboxed.
    let path = documents[0]
        .strip_prefix(&aria_core::tiles::origin())
        .expect("on the sandbox origin");
    let answer = get(port, path).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    let lower = answer.to_lowercase();
    assert!(lower.contains("content-type: application/pdf"), "{answer}");
    assert!(lower.contains("x-content-type-options: nosniff"), "{answer}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settled with the user on 2026-10-29: our own MCP Apps tool is answered
/// without asking. The second half of the test is the part that matters — the
/// exemption is for that tool and nothing else.
#[tokio::test]
async fn showing_a_tile_is_allowed_without_a_prompt_and_nothing_else_is() {
    use std::time::Duration;

    let core = aria_core::Core::new();
    let session = SessionId::new();

    let decision = tokio::time::timeout(
        Duration::from_secs(2),
        core.decide_permission(session, &aria_core::tiles::show_tile_tool(aria_core::providers::McpToolNaming::DoubleUnderscore), None),
    )
    .await
    .expect("a tile is not a question");
    assert_eq!(decision, aria_core::permissions::Decision::Allow);

    // Another tool with nothing to match on waits for a human, for ever —
    // decision 8. Proven by it *not* answering, which is the only way to prove
    // an auto-approval is narrow.
    let still_waiting = tokio::time::timeout(
        Duration::from_millis(300),
        core.decide_permission(session, "mcp__aria__delegate_task", None),
    )
    .await;
    assert!(
        still_waiting.is_err(),
        "seul l'outil de tuile est exempté : {still_waiting:?}"
    );
}

/// A tool the session cannot see is a tool that does not exist.
///
/// Over a pipe, which is where the relay is pure protocol: the token guard is
/// an axum layer in front of the door, not something the handlers check. So
/// this says what a delegated session's `tools/list` returns, and that is the
/// one thing that would silently not happen.
#[tokio::test]
async fn a_session_is_served_the_tile_tool_next_to_the_relay() {
    use rmcp::ServiceExt;

    let core = aria_core::Core::new();
    let (ours, theirs) = tokio::io::duplex(8 * 1024);
    let relay = aria_core::mcp::Relay::new(core);
    tokio::spawn(async move {
        if let Ok(running) = relay.serve(ours).await {
            let _ = running.waiting().await;
        }
    });
    let client = ()
        .serve(theirs)
        .await
        .expect("the client and the server agree on a protocol version");

    let listed = client.list_all_tools().await.expect("tools/list answers");
    let mut names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["approve", "refresh_tile", aria_core::tiles::SHOW_TILE]
    );

    let _ = client.cancel().await;
}

/// A tile that arrived on an endpoint we cannot route is refused rather than
/// shown: the interface has no session to attribute it to.
///
/// The pipe has no HTTP request behind it, so this is also the only cheap way
/// to reach that branch.
#[tokio::test]
async fn a_tile_from_an_unroutable_caller_is_refused() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let core = aria_core::Core::new();
    let held = Arc::clone(core.tiles());
    let (ours, theirs) = tokio::io::duplex(8 * 1024);
    let relay = aria_core::mcp::Relay::new(core);
    tokio::spawn(async move {
        if let Ok(running) = relay.serve(ours).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(theirs).await.expect("a protocol version");

    let result = client
        .call_tool(
            CallToolRequestParams::new(aria_core::tiles::SHOW_TILE).with_arguments(
                serde_json::json!({ "title": "Météo", "html": "<!doctype html><p>23" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .expect("tools/call answers");

    assert!(
        format!("{:?}", result.content).contains("rattacher"),
        "{:?}",
        result.content
    );
    assert_eq!(held.count(), 0, "rien n'a été enregistré");

    let _ = client.cancel().await;
}

/// Serve one tile for twenty seconds, so a browser can be pointed at it.
///
/// The only way to see a tile **without a desktop**, and the only way to check
/// the one thing the policy cannot be reasoned about from the outside: whether
/// the inline script a tile arrives with actually runs under the header the
/// sandbox sends. `script-src 'self'` was written first and blocks it silently
/// — the tile renders and never speaks.
///
/// ```sh
/// cargo test -p aria-core --test tiles -- --ignored --nocapture a_tile_can_be_looked_at
/// chrome --headless=new --disable-gpu --default-background-color=0b1119ff \
///        --window-size=420,320 --virtual-time-budget=6000 \
///        --screenshot=tuile.png "http://127.0.0.1:<port>/t/1"
/// ```
#[tokio::test]
#[ignore = "serves for twenty seconds so a browser can look at it"]
async fn a_tile_can_be_looked_at() {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(
        SessionId::new(),
        "Pièces jointes",
        r#"<p class="label">Audio</p>
<audio id="a" controls src="f/morceau.mp3"></audio>
<p class="label">Vidéo</p>
<video id="v" controls width="300" src="f/clip.mp4"></video>
<p class="label">Document</p>
<embed type="application/pdf" src="f/lettre.pdf" width="300" height="260">
<p class="label">Ce que le navigateur en a fait</p>
<pre id="d" class="note">…</pre>
<script>
  var a = document.getElementById("a");
  var v = document.getElementById("v");
  var d = document.getElementById("d");
  function round(x) { return (x || 0).toFixed(1); }
  function say() {
    d.textContent =
      "mp3 : duree " + round(a.duration) + " s, pret " + a.readyState + "\n" +
      "mp4 : duree " + round(v.duration) + " s, " +
      v.videoWidth + "x" + v.videoHeight + ", pret " + v.readyState + "\n" +
      "mp4 : lu jusqu a " + round(v.currentTime) + " s";
  }
  // Si la CSP refuse quelque chose, elle le dit — et c'est la seule façon de
  // distinguer « la politique a bloqué » de « le navigateur ne sait pas
  // afficher ça ».
  var refus = [];
  document.addEventListener("securitypolicyviolation", function (e) {
    refus.push(e.violatedDirective + " a bloque " + e.blockedURI);
  });
  setInterval(function () {
    say();
    d.textContent += "\ncsp  : " + (refus.length ? refus.join(" ; ") : "aucun refus");
  }, 200);
  a.play().catch(function (e) { d.textContent = "audio refuse : " + e.message; });
  v.play().catch(function (e) { d.textContent = "video refusee : " + e.message; });
</script>"#,
        vec![
            aria_core::tiles::TileFile {
                name: "morceau.mp3".into(),
                path: "D:/MP3/01 - Daft Punk - Harder Better Faster Stronger (Alive radio edit 2007).mp3".into(),
            },
            aria_core::tiles::TileFile {
                name: "clip.mp4".into(),
                path: format!("{home}/Downloads/aria.mp4"),
            },
            aria_core::tiles::TileFile {
                name: "lettre.pdf".into(),
                path: format!(
                    "{home}/Downloads/Lettre_motivation_DINUM_Responsable_produit_IA.pdf"
                ),
            },
        ],
        None,
    );
    let port = sandbox(Arc::clone(&tiles)).await;
    println!("http://127.0.0.1:{port}{}", tile.path());
    tokio::time::sleep(std::time::Duration::from_secs(40)).await;
}

/// `object-src 'self'` buys the PDF viewer, and would have cost tile
/// isolation if nothing else moved.
///
/// Both a PDF and a neighbouring tile's document live on the one sandbox
/// origin, so the policy cannot tell them apart: an `<embed src="/t/2">`
/// would be same-origin and scriptable from `/t/1`. The line is drawn by a
/// header instead — **documents refuse to be embedded, files do not carry
/// it** — and a header only exists on the wire.
#[tokio::test]
async fn a_tile_refuses_to_be_embedded_in_another() {
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(
        SessionId::new(),
        "Météo",
        "<p>23</p>",
        vec![aria_core::tiles::TileFile {
            name: "note.txt".into(),
            path: concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml").into(),
        }],
        None,
    );
    let port = sandbox(Arc::clone(&tiles)).await;

    let document = get(port, &tile.path()).await;
    let policy = document
        .lines()
        .find(|line| {
            line.to_ascii_lowercase()
                .starts_with("content-security-policy:")
        })
        .expect("une CSP sur le document");

    // The window may frame a tile — this is the assertion that was missing on
    // 2026-11-01, and its absence let `X-Frame-Options: DENY` ship: every
    // tile came up empty in the application while every test here passed,
    // because they all loaded a tile as a top-level page.
    for host in aria_core::tiles::HOST_ORIGINS {
        assert!(
            policy.contains(host),
            "la fenêtre doit pouvoir encadrer une tuile — {host} manque dans « {policy} »"
        );
    }
    assert!(policy.contains("frame-ancestors"), "{policy}");
    // And a tile may not: the sandbox origin is not an accepted ancestor.
    assert!(
        !policy.contains(&aria_core::tiles::origin()),
        "une tuile ne doit pas pouvoir en encadrer une autre : « {policy} »"
    );
    // `X-Frame-Options` cannot express that distinction — `DENY` refuses the
    // window too, `SAMEORIGIN` allows the tile — so it must not come back.
    assert!(
        !document.to_ascii_lowercase().contains("x-frame-options"),
        "ce verbe ne sait pas dire « cette fenêtre, pas cette tuile » :\n{document}"
    );

    // And a file carries no ancestor restriction at all, or the PDF viewer —
    // which is a nested browsing context — refuses to render.
    let file = get(port, &format!("{}/f/note.txt", tile.path())).await;
    assert!(file.starts_with("HTTP/1.1 200"), "{file}");
    assert!(
        !file.to_ascii_lowercase().contains("frame-ancestors"),
        "un fichier doit pouvoir être embarqué :\n{file}"
    );
}

/// A name nobody declared is a 404, whatever it contains.
///
/// The containment is the **declaration**, not a check on the path: the
/// request never supplies one. So traversal has nothing to traverse.
#[tokio::test]
async fn only_a_declared_name_resolves_to_a_file() {
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(SessionId::new(), "Doc", "<p>x</p>", Vec::new(), None);
    let port = sandbox(Arc::clone(&tiles)).await;

    for name in ["secret.txt", "..%2F..%2Fetc%2Fpasswd", "Cargo.toml"] {
        let answer = get(port, &format!("{}/f/{name}", tile.path())).await;
        assert!(
            answer.starts_with("HTTP/1.1 404"),
            "« {name} » n'a pas été déclaré et ne doit pas être servi :\n{answer}"
        );
    }
}

/// A `<video>` is not a download: the browser asks for the last bytes to find
/// the index, then jumps. Without ranges, playback starts and the slider does
/// nothing — a symptom that looks like a broken control, not a missing header.
#[tokio::test]
async fn a_range_is_answered_with_the_bytes_that_were_asked_for() {
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(
        SessionId::new(),
        "Média",
        "<p>x</p>",
        vec![aria_core::tiles::TileFile {
            name: "data.bin".into(),
            path: concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml").into(),
        }],
        None,
    );
    let port = sandbox(Arc::clone(&tiles)).await;
    let path = format!("{}/f/data.bin", tile.path());

    let whole = get(port, &path).await;
    assert!(
        whole.to_ascii_lowercase().contains("accept-ranges: bytes"),
        "{whole}"
    );

    let partial = get_with(port, &path, "Range: bytes=0-9\r\n").await;
    assert!(partial.starts_with("HTTP/1.1 206"), "{partial}");
    let line = partial
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-range:"))
        .expect("un Content-Range");
    assert!(line.contains("bytes 0-9/"), "{line}");
    assert!(
        line.ends_with(|c: char| c.is_ascii_digit()),
        "la taille totale est obligatoire : {line}"
    );

    // The form Chromium uses to seek: the last bytes of the file.
    let suffix = get_with(port, &path, "Range: bytes=-16\r\n").await;
    assert!(suffix.starts_with("HTTP/1.1 206"), "{suffix}");
}

/// The half of a refresh that is not the window's: a file rewritten on disk
/// is already the file the tile serves.
///
/// Nothing is copied into the registry — the path stays the session's — so
/// "push an update after modifying it" needs no update of the contents at
/// all. That is worth a test, because the obvious implementation is the one
/// that caches and then has to be invalidated.
#[tokio::test]
async fn a_file_rewritten_on_disk_is_what_the_tile_serves_next() {
    let dir = std::env::temp_dir().join(format!("aria-tile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("un dossier de travail");
    let on_disk = dir.join("rapport.txt");
    std::fs::write(&on_disk, b"premiere version").expect("ecriture");

    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(
        SessionId::new(),
        "Rapport",
        "<p>x</p>",
        vec![aria_core::tiles::TileFile {
            name: "rapport.txt".into(),
            path: on_disk.display().to_string(),
        }],
        None,
    );
    let port = sandbox(Arc::clone(&tiles)).await;
    let path = format!("{}/f/rapport.txt", tile.path());

    assert!(get(port, &path).await.contains("premiere version"));

    // The session reworks the document.
    std::fs::write(&on_disk, b"deuxieme version").expect("reecriture");
    let after = get(port, &path).await;
    assert!(
        after.contains("deuxieme version"),
        "le fichier est servi là où il est, pas copié :\n{after}"
    );
    // And nothing is cached, or the window's reload would show the old one.
    assert!(
        after
            .to_ascii_lowercase()
            .contains("cache-control: no-store")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The event the window needs, and the case that is not an error.
#[tokio::test]
async fn refreshing_says_so_on_the_stream_and_shrugs_at_a_closed_tile() {
    let core = aria_core::Core::new();
    let mut events = core.broker().subscribe();
    let uri = core.show_tile(SessionId::new(), "Rapport", "<p>un</p>", Vec::new(), None);

    assert!(core.refresh_tile(&uri, Some("<p>deux</p>".into())));
    let announced = loop {
        let envelope = events.recv().await.expect("the stream stays open");
        if let aria_core::events::Event::TileUpdated { uri } = envelope.event {
            break uri;
        }
    };
    assert_eq!(announced, uri);
    assert!(
        core.tiles()
            .get(&uri)
            .expect("elle est là")
            .body
            .contains("deux"),
        "le corps a bien été remplacé"
    );

    // A tile the user has closed. A normal outcome, not a failure.
    core.tiles().remove(&uri);
    assert!(!core.refresh_tile(&uri, None));
}

/// The window has to be able to say « the sandbox is down » rather than let
/// the browser render its own error page inside the panel.
///
/// Seen on 2026-11-03: every tile showed « 127.0.0.1 a refusé de se
/// connecter », which reads as broken tiles when the truth was a port nobody
/// was listening on. The window cannot find out by itself — its CSP lets it
/// reach the event stream and nothing else — so the core carries it.
#[tokio::test]
async fn the_snapshot_says_whether_the_sandbox_is_up() {
    let core = aria_core::Core::new();
    // Nothing has bound anything yet, and that is the honest default: a core
    // on its own serves no origin.
    assert!(!core.snapshot().tiles_served);

    core.tiles().set_serving(true);
    assert!(core.snapshot().tiles_served);

    // And a server that dies later says so too.
    core.tiles().set_serving(false);
    assert!(!core.snapshot().tiles_served);
}

/// Every tile the sandbox serves carries the one line that proves it arrived.
///
/// A cross-origin frame fires `load` for the browser's error page exactly as
/// it does for a document, so the window cannot tell « served » from
/// « refused » by watching. This note can only be sent by a document that was
/// served, parsed and run — which is why it is injected by the wrapper and
/// not left to the session to remember.
#[tokio::test]
async fn a_served_tile_announces_itself_to_the_window() {
    let tiles = Arc::new(Registry::default());
    let tile = tiles.add(SessionId::new(), "Météo", "<p>23</p>", Vec::new(), None);
    let port = sandbox(Arc::clone(&tiles)).await;

    let page = get(port, &tile.path()).await;
    assert!(page.contains("tile.arrived"), "{page}");
    assert!(
        page.contains("parent.postMessage"),
        "la note doit remonter au parent : {page}"
    );
    // And it must survive a body that throws: a tile whose own script fails
    // is still a tile that arrived.
    assert!(page.contains("try{"), "{page}");
}
