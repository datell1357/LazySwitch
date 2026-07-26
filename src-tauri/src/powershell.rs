use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

pub fn encode(script: &str) -> String {
    let utf16le: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    STANDARD.encode(utf16le)
}

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub async fn exec_file_text(file: &str, args: &[&str], timeout_ms: u64) -> Result<String, String> {
    let mut cmd = Command::new(file);
    cmd.args(args);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = match timeout(Duration::from_millis(timeout_ms), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err(format!("{file} timed out after {timeout_ms}ms")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{file} exited with {:?}", output.status.code())
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Runs `script` via `powershell.exe -EncodedCommand` and returns raw stdout.
pub async fn run(exe: &str, script: &str, timeout_ms: u64) -> Result<String, String> {
    exec_file_text(
        exe,
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encode(script),
        ],
        timeout_ms,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_round_trips_utf16le_base64() {
        let encoded = encode("Write-Output 'hi'");
        let bytes = STANDARD.decode(encoded).unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "Write-Output 'hi'");
    }

    #[test]
    fn quote_escapes_single_quotes() {
        assert_eq!(quote("it's"), "'it''s'");
    }
}
