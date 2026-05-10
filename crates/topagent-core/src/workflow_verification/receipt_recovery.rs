use super::command_match::{bash_command_family, classify_bash_command};
use crate::behavior::BashCommandClass;
use crate::receipt_index::{bash_command_from_receipt, is_successful_receipt};
use crate::task_result::{ToolActionOutcome, ToolActionReceipt};

pub(crate) fn unrecovered_receipt_issues(receipts: &[ToolActionReceipt]) -> Vec<String> {
    receipts
        .iter()
        .enumerate()
        .filter(|(index, receipt)| {
            receipt.outcome != ToolActionOutcome::Succeeded
                && !has_later_recovery_receipt(receipts, *index, receipt)
        })
        .map(|(_, receipt)| {
            format!(
                "Unrecovered tool {}: {} - {}",
                receipt.outcome.label(),
                receipt.tool_name,
                receipt.summary
            )
        })
        .collect()
}

fn has_later_recovery_receipt(
    receipts: &[ToolActionReceipt],
    index: usize,
    receipt: &ToolActionReceipt,
) -> bool {
    receipts.iter().skip(index + 1).any(|later| {
        is_successful_receipt(later)
            && later.tool_name == receipt.tool_name
            && receipt_recovery_key(receipt).is_some_and(|failed_key| {
                receipt_recovery_key(later).is_some_and(|later_key| later_key == failed_key)
            })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReceiptRecoveryKey {
    Path { tool: String, path: String },
    Bash { family: String, verification: bool },
    Summary { tool: String, summary: String },
}

fn receipt_recovery_key(receipt: &ToolActionReceipt) -> Option<ReceiptRecoveryKey> {
    match receipt.tool_name.as_str() {
        "bash" => bash_command_from_receipt(receipt).map(|command| {
            let verification = classify_bash_command(command) == BashCommandClass::Verification;
            ReceiptRecoveryKey::Bash {
                family: bash_command_family(command),
                verification,
            }
        }),
        "read" | "write" | "edit" | "external_send" => {
            receipt_target(&receipt.summary, &receipt.tool_name).map(|path| {
                ReceiptRecoveryKey::Path {
                    tool: receipt.tool_name.clone(),
                    path,
                }
            })
        }
        _ => (!receipt.summary.trim().is_empty()).then(|| ReceiptRecoveryKey::Summary {
            tool: receipt.tool_name.clone(),
            summary: receipt.summary.clone(),
        }),
    }
}

fn receipt_target(summary: &str, tool_name: &str) -> Option<String> {
    let prefix = format!("{tool_name}: ");
    summary
        .strip_prefix(&prefix)
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(ToOwned::to_owned)
}
