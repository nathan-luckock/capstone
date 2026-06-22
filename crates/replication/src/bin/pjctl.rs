//! pjctl: a command-line client for a running picklejar replication cluster.
//!
//! Point it at the cluster's nodes and put/get raw keys or store/recall vector
//! memories. Pair it with `pjnode` servers:
//! ```text
//! pjctl --node 0@127.0.0.1:7000 --node 1@127.0.0.1:7001 --node 2@127.0.0.1:7002 \
//!       store 1 0.1,0.2,0.9 "the sky is blue"
//! pjctl --node 0@127.0.0.1:7000 ... recall 0.1,0.2,0.85 5
//! ```
//! Commands:
//!   put <key:u64> <value>
//!   get <key:u64>
//!   store <id:u64> <f32,f32,...> <content>
//!   recall <f32,f32,...> <k>

use picklejar_replication::net::Coordinator;

fn parse_vec(s: &str) -> Vec<f32> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut nodes: Vec<(u64, String)> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--node" {
            i += 1;
            if let Some((id, addr)) = args.get(i).and_then(|s| s.split_once('@')) {
                if let Ok(id) = id.parse::<u64>() {
                    nodes.push((id, addr.to_string()));
                }
            }
        } else {
            rest.push(args[i].clone());
        }
        i += 1;
    }

    if nodes.is_empty() {
        eprintln!("need at least one --node id@addr");
        std::process::exit(2);
    }
    let rf = 3.min(nodes.len());
    let quorum = (rf / 2) + 1;
    let coord = Coordinator::new(&nodes, rf, quorum, quorum);

    match rest.first().map_or("", String::as_str) {
        "put" => match (rest.get(1).and_then(|s| s.parse::<u64>().ok()), rest.get(2)) {
            (Some(key), Some(value)) => {
                let acks = coord.write(key, value.as_bytes());
                println!("put key {key}: {acks} replica(s) acked");
            }
            _ => fail("usage: put <key> <value>"),
        },
        "get" => match rest.get(1).and_then(|s| s.parse::<u64>().ok()) {
            Some(key) => match coord.read(key) {
                Some(v) => println!("{}", String::from_utf8_lossy(&v)),
                None => println!("<none>"),
            },
            None => fail("usage: get <key>"),
        },
        "store" => {
            let tenant = rest.get(1).map_or("", String::as_str);
            let id = rest.get(2).and_then(|s| s.parse::<u64>().ok());
            let emb = rest.get(3).map(|s| parse_vec(s));
            let content = rest.get(4).map_or("", String::as_str);
            match (tenant.is_empty(), id, emb) {
                (false, Some(id), Some(emb)) if !emb.is_empty() => {
                    let acks = coord.store_memory(tenant, id, &emb, content.as_bytes());
                    println!(
                        "stored memory {id} for tenant {tenant} (dim {}): {acks} replica(s) acked",
                        emb.len()
                    );
                }
                _ => fail("usage: store <tenant> <id> <f,f,...> <content>"),
            }
        }
        "recall" => {
            let tenant = rest.get(1).map_or("", String::as_str);
            let query = rest.get(2).map(|s| parse_vec(s));
            let k = rest
                .get(3)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(5);
            match query {
                Some(q) if !q.is_empty() && !tenant.is_empty() => {
                    let hits = coord.recall(tenant, &q, k);
                    if hits.is_empty() {
                        println!("(no memories)");
                    }
                    for h in hits {
                        println!(
                            "  id {:>4}  dist {:>10.4}  {}",
                            h.id,
                            h.distance,
                            String::from_utf8_lossy(&h.payload)
                        );
                    }
                }
                _ => fail("usage: recall <tenant> <f,f,...> <k>"),
            }
        }
        other => fail(&format!(
            "unknown command {other:?}; commands: put, get, store, recall"
        )),
    }
}

fn fail(msg: &str) {
    eprintln!("{msg}");
    std::process::exit(2);
}
