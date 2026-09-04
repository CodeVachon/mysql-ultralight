//! Asks two servers what `COM_STMT_PREPARE` answers, without executing.
//!
//! Scratch tool for #0244. `scripts/ask.sh` uses the mysql CLI, which never
//! prepares, so a claim about prepare-time behaviour cannot be measured with
//! it. Two client traps recorded by earlier work and avoided here: the crate
//! caches prepared statements by SQL text and refreshes the cached columns
//! from the *execute* response, so the cache is off; and a statement the
//! client cannot read desynchronises the socket, so every statement gets a
//! fresh connection.
use mysql::prelude::*;
use mysql::{Conn, Opts};

fn probe(url: &str, sql: &str) -> String {
    let opts = match Opts::from_url(url) {
        Ok(o) => o,
        Err(e) => return format!("url: {e}"),
    };
    let mut conn = match Conn::new(opts) {
        Ok(c) => c,
        Err(e) => return format!("connect: {e}"),
    };
    match conn.prep(sql) {
        Ok(stmt) => format!("prepared, {} column(s)", stmt.columns().len()),
        Err(mysql::Error::MySqlError(e)) => format!("{} {}", e.code, e.message),
        Err(e) => format!("other: {e}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (left, right) = (&args[0], &args[1]);
    for sql in args[2..].iter() {
        let a = probe(left, sql);
        let b = probe(right, sql);
        let mark = if a == b { "same" } else { "DIFF" };
        println!("{mark}  {sql}\n      mysql  {a}\n      engine {b}");
    }
}
