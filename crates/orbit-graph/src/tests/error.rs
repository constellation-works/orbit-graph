use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use orbit_graph_extract::ExtractError;

use crate::{GraphError, GraphErrorClass};

#[test]
fn extract_errors_keep_their_class_across_the_crate_split() {
    let git = GraphError::from(ExtractError::Git {
        operation: "load before commit",
        source: git2::Error::from_str("object not found"),
    });
    assert_eq!(git.class(), GraphErrorClass::Git);
    assert_eq!(git.to_string(), "load before commit: object not found");
    assert!(
        std::error::Error::source(&git).is_some(),
        "the git2 error stays the source"
    );

    let invalid = GraphError::from(ExtractError::InvalidData {
        operation: "validate timestamp",
        reason: "cutoff must be RFC 3339 or unix:<seconds>".into(),
    });
    assert_eq!(invalid.class(), GraphErrorClass::InvalidData);
    assert_eq!(
        invalid.to_string(),
        "validate timestamp: cutoff must be RFC 3339 or unix:<seconds>"
    );

    let io = GraphError::from(ExtractError::Io {
        operation: "canonicalize history repository",
        path: PathBuf::from("/missing"),
        source: std::io::Error::from(ErrorKind::NotFound),
    });
    assert_eq!(io.class(), GraphErrorClass::Io { transient: false });
    assert_eq!(
        io.to_string(),
        GraphError::io(
            "canonicalize history repository",
            "/missing",
            std::io::Error::from(ErrorKind::NotFound)
        )
        .to_string()
    );
}

#[test]
fn each_failure_class_is_distinct_and_only_interruptions_are_transient() {
    let transient = GraphError::io("read", "/x", std::io::Error::from(ErrorKind::Interrupted));
    assert_eq!(transient.class(), GraphErrorClass::Io { transient: true });
    let timed_out = GraphError::io("read", "/x", std::io::Error::from(ErrorKind::TimedOut));
    assert_eq!(timed_out.class(), GraphErrorClass::Io { transient: true });

    let classes = [
        GraphError::invalid_input("parse selector", "selector", "bad").class(),
        GraphError::not_found("resolve revision", "revision \"x\"").class(),
        GraphError::git("open", git2::Error::from_str("broken")).class(),
        GraphError::sqlite_message("query", "locked").class(),
        GraphError::invalid_data("decode", "bad").class(),
        GraphError::timeout("lock", Duration::from_millis(5), "held").class(),
        GraphError::orbit_refused("orbit.task.show", "task_not_found", "no such task").class(),
        GraphError::subprocess("run orbit", "signal 9", "").class(),
    ];
    for (index, class) in classes.iter().enumerate() {
        for other in &classes[index + 1..] {
            assert_ne!(class, other);
        }
    }
}

#[test]
fn a_timeout_and_an_orbit_refusal_say_what_happened() {
    assert_eq!(
        GraphError::timeout("acquire lock", Duration::from_millis(250), "held by pid 7")
            .to_string(),
        "acquire lock: timed out after 250 ms; held by pid 7"
    );
    let refused = GraphError::orbit_refused("orbit.task.show", "task_not_found", "no such task");
    assert_eq!(
        refused.to_string(),
        "orbit.task.show refused: task_not_found: no such task"
    );
    let GraphError::OrbitRefused { code, .. } = refused else {
        panic!("an Orbit refusal keeps its variant");
    };
    assert_eq!(code, "task_not_found");
}
