use std::time::Instant;

use orbit_graph::{Graph, ImpactDirection, RefConfidence, RefOpts, Selector, SyncPolicy};
use rusqlite::{Connection, params};

fn median_us(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() {
    let root = tempfile::tempdir().unwrap();
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = git2::Repository::init_opts(root.path(), &options).unwrap();
    repo.config()
        .unwrap()
        .set_str("core.excludesFile", "/dev/null")
        .unwrap();
    std::fs::write(root.path().join("target.rs"), "fn target() {}\n").unwrap();
    std::fs::write(root.path().join("caller.rs"), "fn caller() { target(); }\n").unwrap();
    let graph = Graph::open(root.path(), SyncPolicy::Manual).unwrap();
    let mut conn = Connection::open(graph.db_path().path()).unwrap();
    println!(
        "public_api_sqlite_version={}",
        conn.query_row("SELECT sqlite_version()", [], |row| row.get::<_, String>(0))
            .unwrap()
    );
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO files VALUES ('target.rs', x'00', 1, 'rust', 15, 2)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO files VALUES ('caller.rs', x'00', 1, 'rust', 25, 2)",
        [],
    )
    .unwrap();
    tx.execute("INSERT INTO symbols (id,file_path,name,qualified,kind,span_start,span_end) VALUES (1,'target.rs','target','crate::target','function',0,15)", []).unwrap();
    tx.execute("INSERT INTO symbols (id,file_path,name,qualified,kind,span_start,span_end) VALUES (2,'caller.rs','caller','crate::caller','function',0,25)", []).unwrap();
    {
        let mut insert = tx.prepare("INSERT INTO refs (from_file,from_span_start,from_span_end,target_name,target_qualified,target_symbol_hint,kind,confidence) VALUES ('caller.rs',14,20,?1,?2,?3,'call','exact')").unwrap();
        for index in 0..300_000 {
            if index == 123_456 {
                insert
                    .execute(params!["target", "crate::target", 1])
                    .unwrap();
            } else {
                insert
                    .execute(params![
                        "other",
                        format!("crate::other{}", index % 100),
                        None::<i64>
                    ])
                    .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    let selector = Selector::Symbol {
        path: "target.rs".into(),
        symbol: "target".into(),
        kind: "function".into(),
    };
    println!("refs=300000 matching=1 runs=30 statistic=median units=microseconds");
    for confidence in [RefConfidence::Exact, RefConfidence::FuzzyName] {
        let opts = RefOpts {
            confidence,
            kind: None,
        };
        assert_eq!(graph.refs(&selector, &opts).unwrap().refs.len(), 1);
        let mut elapsed = Vec::new();
        for _ in 0..30 {
            let started = Instant::now();
            let result = graph.refs(&selector, &opts).unwrap();
            elapsed.push(started.elapsed().as_secs_f64() * 1_000_000.0);
            assert_eq!(result.refs.len(), 1);
        }
        println!("refs_{confidence:?}_median_us={:.3}", median_us(elapsed));
        assert_eq!(
            graph
                .impact_with_direction(&selector, 1, confidence, ImpactDirection::Inbound)
                .unwrap()
                .visited_nodes,
            1
        );
        let mut elapsed = Vec::new();
        for _ in 0..30 {
            let started = Instant::now();
            let result = graph
                .impact_with_direction(&selector, 1, confidence, ImpactDirection::Inbound)
                .unwrap();
            elapsed.push(started.elapsed().as_secs_f64() * 1_000_000.0);
            assert_eq!(result.visited_nodes, 1);
        }
        println!("impact_{confidence:?}_median_us={:.3}", median_us(elapsed));
    }
}
