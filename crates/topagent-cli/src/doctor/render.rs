use crate::commands::surface::PRODUCT_NAME;
use crate::doctor::types::{CheckLevel, CheckResult};

pub(crate) fn print_report(checks: &[CheckResult]) {
    println!("{PRODUCT_NAME} doctor");
    println!("{}", "-".repeat(40));

    let mut ok_count = 0usize;
    let mut warn_count = 0usize;
    let mut err_count = 0usize;

    for check in checks {
        match check.level {
            CheckLevel::Ok => ok_count += 1,
            CheckLevel::Warning => warn_count += 1,
            CheckLevel::Error => err_count += 1,
        }

        println!("[{}] {}: {}", check.level.label(), check.name, check.detail);
        if let Some(hint) = &check.hint {
            println!("      hint: {}", hint);
        }
    }

    println!("{}", "-".repeat(40));
    println!(
        "Summary: {} OK, {} warning(s), {} error(s)",
        ok_count, warn_count, err_count
    );
}
