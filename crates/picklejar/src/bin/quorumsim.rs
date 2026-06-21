//! Quorum-replicated memory: the cluster stays available and consistent while a
//! node fails and heals.
//!
//! ```text
//! cargo run --release --bin quorumsim
//! ```

use std::process::ExitCode;

use picklejar::quorum::Cluster;

fn show(label: &str, v: &Result<Option<Vec<u8>>, picklejar::quorum::QuorumError>) -> String {
    match v {
        Ok(Some(bytes)) => format!("{label} = \"{}\"", String::from_utf8_lossy(bytes)),
        Ok(None) => format!("{label} = <none>"),
        Err(e) => format!("{label} = ERROR {e:?}"),
    }
}

fn main() -> ExitCode {
    println!("\n=============== QUORUM-REPLICATED MEMORY ===============");
    println!("5 nodes, rf=3, r=2, w=2  (r + w = 4 > 3, so quorums overlap)\n");

    let mut cluster = Cluster::new(5, 3, 2, 2);
    let key = 0xA11CE;
    let pref = cluster.preference(key);
    println!("memory {key:#x} lives on nodes {pref:?}\n");

    cluster.write(key, b"acme launch codes v1").unwrap();
    println!(
        "write v1 (all 3 replicas): {}",
        show("read", &cluster.read(key))
    );

    // A node holding this memory goes dark.
    println!("\nnode {} fails...", pref[0]);
    cluster.fail(pref[0]);
    let w2 = cluster.write(key, b"acme launch codes v2");
    println!(
        "  write v2 to the survivors: {}",
        if w2.is_ok() {
            "ACK (quorum met)"
        } else {
            "FAILED"
        }
    );
    println!(
        "  {}  <- still available, still latest",
        show("read", &cluster.read(key))
    );

    // The node comes back, stale, and a read repairs it.
    println!("\nnode {} heals (stale)...", pref[0]);
    cluster.heal(pref[0]);
    let synced_before = cluster.replicas_in_sync(key);
    let read_back = cluster.read(key);
    let synced_after = cluster.replicas_in_sync(key);
    println!("  replicas current: {synced_before} -> {synced_after} after a read (read-repair)");
    println!("  {}", show("read", &read_back));

    println!("\n==================================================");
    let ok =
        w2.is_ok() && read_back == Ok(Some(b"acme launch codes v2".to_vec())) && synced_after == 3;
    if ok {
        println!("VERDICT: writes and reads succeeded throughout a node failure, every read");
        println!("returned the latest value (r + w > rf guarantees the overlap), and the");
        println!("healed node was caught up by read-repair. a Dynamo, in miniature, proven.");
    } else {
        println!(
            "VERDICT: unexpected (w2 ok={}, read={read_back:?}, synced={synced_after}).",
            w2.is_ok()
        );
        return ExitCode::FAILURE;
    }
    println!("==================================================\n");
    ExitCode::SUCCESS
}
