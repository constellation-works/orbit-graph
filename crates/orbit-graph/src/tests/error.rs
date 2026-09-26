use std::io::ErrorKind;
use std::path::PathBuf;

use orbit_graph_extract::ExtractError;

use crate::GraphError;

#[test]
fn extract_errors_keep_the_graph_error_they_raised_before_the_crate_split() {
    let git = GraphError::from(ExtractError::Git {
        operation: "load before commit",
        source: git2::Error::from_str("object not found"),
    });
    assert_eq!(
        git,
        GraphError::invalid_data(
            "load before commit",
            git2::Error::from_str("object not found").to_string()
        )
    );

    let invalid = GraphError::from(ExtractError::InvalidData {
        operation: "validate timestamp",
        reason: "cutoff must be RFC 3339 or unix:<seconds>".into(),
    });
    assert_eq!(
        invalid.to_string(),
        "validate timestamp: cutoff must be RFC 3339 or unix:<seconds>"
    );

    let io = GraphError::from(ExtractError::Io {
        operation: "canonicalize history repository",
        path: PathBuf::from("/missing"),
        source: std::io::Error::from(ErrorKind::NotFound),
    });
    assert_eq!(
        io,
        GraphError::io(
            "canonicalize history repository",
            "/missing",
            std::io::Error::from(ErrorKind::NotFound)
        )
    );
}
