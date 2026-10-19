//! Runnable synthetic monitor for the fund -> quote -> settle happy path.
//!
//! This uses the repository's credential-free, in-process sandbox adapters. It
//! never contacts Soroban RPC or submits a transaction to a network.

use std::time::Instant;

use soroban_sdk::{testutils::Address as _, Address, Env, Symbol};
use testing::sandbox::{Sandbox, SandboxConfig};

const AMOUNT: i128 = 100_000;
const ORACLE_PRICE: i128 = 1_000_000;
const EXPECTED_FEE: i128 = 500;

fn main() {
    let started = Instant::now();
    let env = Env::default();
    let payer = Address::generate(&env);
    let payee = Address::generate(&env);
    let asset = Symbol::new(&env, "synthetic_asset");
    let mut sandbox = match Sandbox::new(&env, SandboxConfig::new(0x247)) {
        Ok(sandbox) => sandbox,
        Err(error) => {
            emit_failure(started, "sandbox", "initialize", &format!("{error:?}"));
            std::process::exit(1);
        }
    };

    if let Err(error) = sandbox.token_mut().mint(&payer, AMOUNT) {
        emit_failure(started, "token", "fund payer", &format!("{error:?}"));
        std::process::exit(1);
    }
    if let Err(error) = sandbox.oracle_mut().set_price(&asset, ORACLE_PRICE) {
        emit_failure(started, "oracle", "configure quote", &format!("{error:?}"));
        std::process::exit(1);
    }

    match sandbox.run_primary_workflow(&env, &payer, &payee, &asset, AMOUNT) {
        Ok(report)
            if report.fee == EXPECTED_FEE
                && report.oracle_price == ORACLE_PRICE
                && report.payer_balance == EXPECTED_FEE
                && report.payee_balance == AMOUNT - EXPECTED_FEE
                && report.credential_free =>
        {
            println!(
                "{{\"status\":\"pass\",\"journey\":\"fund_quote_settle\",\"latency_ms\":{},\"steps\":{},\"settled_ledger\":{},\"dependency\":null}}",
                started.elapsed().as_millis(),
                report.steps,
                report.settled_ledger,
            );
        }
        Ok(report) => {
            emit_failure(
                started,
                "workflow_assertion",
                "verify settlement",
                &format!(
                    "unexpected report: fee={}, price={}, payer={}, payee={}, credential_free={}",
                    report.fee,
                    report.oracle_price,
                    report.payer_balance,
                    report.payee_balance,
                    report.credential_free
                ),
            );
            std::process::exit(1);
        }
        Err(error) => {
            let blame = sandbox
                .calls()
                .iter()
                .find(|call| !call.allowed)
                .map(|call| format!("{:?}.{:?}", call.adapter, call.operation))
                .unwrap_or_else(|| "workflow".to_string());
            emit_failure(started, &blame, "run primary workflow", &format!("{error:?}"));
            std::process::exit(1);
        }
    }
}

fn emit_failure(started: Instant, dependency: &str, step: &str, detail: &str) {
    println!(
        "{{\"status\":\"fail\",\"journey\":\"fund_quote_settle\",\"latency_ms\":{},\"failed_step\":{},\"dependency\":{},\"detail\":{}}}",
        started.elapsed().as_millis(),
        json_string(step),
        json_string(dependency),
        json_string(detail),
    );
}

fn json_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            c if c.is_control() => encoded.push(' '),
            c => encoded.push(c),
        }
    }
    encoded.push('"');
    encoded
}
