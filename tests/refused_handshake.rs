//! Drives the binary as a subprocess against a shepherd that refuses its
//! handshake, and reads the exit code back off the real process.
//!
//! The code is the whole point of the test. `std::process::ExitCode`
//! implements neither `PartialEq` nor any way to read the number back, so
//! nothing inside the binary can assert on what `run::refused` returns;
//! the only honest reader is the operating system, which is the same
//! reader shep has. `tests/probe.rs` spawns this binary for the same
//! reason, one layer up.
//!
//! `6` is spelled out here rather than shared with `run::PROTOCOL_MISMATCH`,
//! and not only because a binary crate has no library target to import it
//! from. The number is a contract with shep, written down in that
//! project's `docs/dogs.md`: a dog refused on protocol-version skew exits
//! `6`. A test that read the constant would agree with the binary about
//! any number at all, including a wrong one.
//!
//! `SHEP_HOME` points at a fresh temporary directory, so the fake shepherd
//! binds a socket there and this test never reaches the real one running
//! under the developer's own `$SHEP_HOME`.

use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use shep_client::{
    shep_core::protocol::{RpcError, RpcErrorCode},
    testing::fake_daemon,
};
use tempfile::TempDir;

/// How long the dog is given to connect, be refused, and exit.
///
/// Generous, because it is a bound rather than an expectation: the whole
/// sequence is one connect and one frame each way over a unix socket, and
/// it takes milliseconds. What the bound buys is a test that fails with a
/// sentence instead of hanging a CI run when a future change makes the
/// refusal non-fatal again.
const EXIT_BUDGET: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_handshake_exits_on_sheps_own_protocol_mismatch_code() {
    let home = TempDir::new().expect("temp dir");
    let run = home.path().join("run");
    std::fs::create_dir_all(&run).expect("run dir");

    // Refused the way a shepherd refuses a dog it will not talk to: a
    // `HelloReply` carrying `ProtocolMismatch` and the daemon's own
    // version, which is the one place a client can learn that version.
    let shepherd = fake_daemon(
        &run.join("shep.sock"),
        Err(RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "protocol 9 is below this shepherd's floor".to_owned(),
            daemon_version: Some("9.9.9".to_owned()),
        }),
    )
    .await;

    let mut dog = Command::new(env!("CARGO_BIN_EXE_shep-discord"))
        .env("SHEP_HOME", home.path())
        // Named, so it takes the `connect_as_dog` path an adopted dog
        // takes. A nameless one is refused the same way, but it is not
        // the run this code exists for.
        .env("SHEP_DOG_NAME", "chatter")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawned");

    let deadline = Instant::now() + EXIT_BUDGET;
    let status = loop {
        if let Some(status) = dog.try_wait().expect("waited on the dog") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the dog was still running {EXIT_BUDGET:?} after a refused handshake"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let mut stderr = String::new();
    dog.stderr
        .take()
        .expect("piped")
        .read_to_string(&mut stderr)
        .expect("utf8");

    assert_eq!(status.code(), Some(6), "{stderr}");
    // Read as well as the code, so a `6` arriving from some other path
    // could not pass for this one.
    assert!(stderr.contains("refused this dog's handshake"), "{stderr}");
    assert!(stderr.contains("9.9.9"), "{stderr}");

    let hello = shepherd.await.expect("the fake shepherd heard a Hello");
    assert_eq!(hello.dog_name.as_deref(), Some("chatter"));
}
