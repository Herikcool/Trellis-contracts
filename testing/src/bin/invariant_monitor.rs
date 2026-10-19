use shared::invariants::{AssetConservationSnapshot, InvariantMonitor, MonitorReport};

fn main() {
    println!("Running Trellis Invariant Monitor...");

    // Simulated before and after states
    let before = AssetConservationSnapshot {
        ledger_sequence: 100,
        external_accounts: vec![],
        contract_token_reserve: 1000,
        internal_liabilities: 1000,
    };
    
    let after = AssetConservationSnapshot {
        ledger_sequence: 101,
        external_accounts: vec![],
        contract_token_reserve: 1000,
        internal_liabilities: 1000,
    };

    let report1 = InvariantMonitor::check_conservation(&before, &after);
    print_report(&report1);

    let report2 = InvariantMonitor::check_ownership("record_123".to_string(), "owner_A".to_string(), "owner_A".to_string());
    print_report(&report2);

    let report3 = InvariantMonitor::check_lifecycle("record_456".to_string(), true, "Active".to_string());
    print_report(&report3);

    let report4 = InvariantMonitor::check_authorization("record_789".to_string(), true, "user_1".to_string(), "withdraw".to_string());
    print_report(&report4);

    println!("Monitor run complete.");
}

fn print_report(report: &MonitorReport) {
    if report.passed {
        println!("[PASS] {}", report.invariant_name);
    } else {
        if let Some(v) = &report.violation {
            let sev = v.severity();
            let rem = v.remediation();
            println!("[FAIL] {} (Severity: {:?}) - {:?}", report.invariant_name, sev, v);
            println!("       Recommended Action: {}", rem);
        }
    }
}
