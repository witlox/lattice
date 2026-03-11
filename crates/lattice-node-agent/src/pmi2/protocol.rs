//! PMI-2 wire protocol parser and serializer.
//!
//! The PMI-2 protocol is text-based over a Unix domain socket. Each message
//! is a semicolon-delimited set of key=value pairs terminated by a newline.
//! Format: `cmd=<command>;key1=val1;key2=val2;\n`

use std::collections::HashMap;

/// Parsed PMI-2 command from a rank process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pmi2Command {
    /// Initialize PMI connection.
    /// Fields: pmi_version, pmi_subversion
    FullInit {
        pmi_version: u32,
        pmi_subversion: u32,
    },
    /// Query job attributes.
    /// Fields: key (attribute name)
    JobGetInfo { key: String },
    /// Store a key-value pair in the local KVS.
    KvsPut { key: String, value: String },
    /// Retrieve a key-value pair from the KVS.
    KvsGet { key: String },
    /// Barrier + distribute all KV pairs across all ranks.
    KvsFence,
    /// Clean shutdown of PMI connection.
    Finalize,
    /// Signal abnormal termination.
    Abort { message: String },
}

/// Response sent back to a rank process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pmi2Response {
    /// Response to fullinit.
    FullInitResp {
        rank: u32,
        size: u32,
        appnum: u32,
        pmi_version: u32,
        pmi_subversion: u32,
        spawner_jobid: String,
        rc: i32,
    },
    /// Response to job-getinfo.
    JobGetInfoResp {
        key: String,
        value: String,
        found: bool,
        rc: i32,
    },
    /// Response to kvsput.
    KvsPutResp { rc: i32 },
    /// Response to kvsget.
    KvsGetResp {
        key: String,
        value: String,
        found: bool,
        rc: i32,
    },
    /// Response to kvsfence.
    KvsFenceResp { rc: i32 },
    /// Response to finalize.
    FinalizeResp { rc: i32 },
}

/// Parse a PMI-2 wire message into a command.
///
/// The format is: `cmd=<command>;key=value;...\n`
pub fn parse_command(line: &str) -> Result<Pmi2Command, String> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    let fields = parse_fields(line);

    let cmd = fields
        .get("cmd")
        .ok_or_else(|| "missing cmd field".to_string())?;

    match cmd.as_str() {
        "fullinit" => {
            let pmi_version = fields
                .get("pmi_version")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2);
            let pmi_subversion = fields
                .get("pmi_subversion")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Ok(Pmi2Command::FullInit {
                pmi_version,
                pmi_subversion,
            })
        }
        "job-getinfo" => {
            let key = fields
                .get("key")
                .cloned()
                .ok_or_else(|| "job-getinfo: missing key".to_string())?;
            Ok(Pmi2Command::JobGetInfo { key })
        }
        "kvsput" => {
            let key = fields
                .get("key")
                .cloned()
                .ok_or_else(|| "kvsput: missing key".to_string())?;
            let value = fields
                .get("value")
                .cloned()
                .ok_or_else(|| "kvsput: missing value".to_string())?;
            Ok(Pmi2Command::KvsPut { key, value })
        }
        "kvsget" => {
            let key = fields
                .get("key")
                .cloned()
                .ok_or_else(|| "kvsget: missing key".to_string())?;
            Ok(Pmi2Command::KvsGet { key })
        }
        "kvsfence" => Ok(Pmi2Command::KvsFence),
        "finalize" => Ok(Pmi2Command::Finalize),
        "abort" => {
            let message = fields.get("message").cloned().unwrap_or_default();
            Ok(Pmi2Command::Abort { message })
        }
        other => Err(format!("unknown PMI-2 command: {other}")),
    }
}

/// Serialize a PMI-2 response to wire format.
pub fn format_response(resp: &Pmi2Response) -> String {
    match resp {
        Pmi2Response::FullInitResp {
            rank,
            size,
            appnum,
            pmi_version,
            pmi_subversion,
            spawner_jobid,
            rc,
        } => {
            format!(
                "cmd=fullinit-response;rc={rc};rank={rank};size={size};\
                 appnum={appnum};pmi_version={pmi_version};\
                 pmi_subversion={pmi_subversion};spawner-jobid={spawner_jobid};\n"
            )
        }
        Pmi2Response::JobGetInfoResp {
            key,
            value,
            found,
            rc,
        } => {
            format!("cmd=job-getinfo-response;rc={rc};key={key};value={value};found={found};\n")
        }
        Pmi2Response::KvsPutResp { rc } => {
            format!("cmd=kvsput-response;rc={rc};\n")
        }
        Pmi2Response::KvsGetResp {
            key,
            value,
            found,
            rc,
        } => {
            format!("cmd=kvsget-response;rc={rc};key={key};value={value};found={found};\n")
        }
        Pmi2Response::KvsFenceResp { rc } => {
            format!("cmd=kvsfence-response;rc={rc};\n")
        }
        Pmi2Response::FinalizeResp { rc } => {
            format!("cmd=finalize-response;rc={rc};\n")
        }
    }
}

/// Parse semicolon-delimited key=value pairs.
fn parse_fields(line: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for part in line.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            fields.insert(k.to_string(), v.to_string());
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fullinit() {
        let cmd = parse_command("cmd=fullinit;pmi_version=2;pmi_subversion=0;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::FullInit {
                pmi_version: 2,
                pmi_subversion: 0,
            }
        );
    }

    #[test]
    fn parse_fullinit_defaults() {
        let cmd = parse_command("cmd=fullinit;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::FullInit {
                pmi_version: 2,
                pmi_subversion: 0,
            }
        );
    }

    #[test]
    fn parse_kvsput() {
        let cmd = parse_command("cmd=kvsput;key=mykey;value=myval;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::KvsPut {
                key: "mykey".to_string(),
                value: "myval".to_string(),
            }
        );
    }

    #[test]
    fn parse_kvsget() {
        let cmd = parse_command("cmd=kvsget;key=mykey;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::KvsGet {
                key: "mykey".to_string(),
            }
        );
    }

    #[test]
    fn parse_kvsfence() {
        let cmd = parse_command("cmd=kvsfence;\n").unwrap();
        assert_eq!(cmd, Pmi2Command::KvsFence);
    }

    #[test]
    fn parse_finalize() {
        let cmd = parse_command("cmd=finalize;\n").unwrap();
        assert_eq!(cmd, Pmi2Command::Finalize);
    }

    #[test]
    fn parse_abort() {
        let cmd = parse_command("cmd=abort;message=something went wrong;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::Abort {
                message: "something went wrong".to_string(),
            }
        );
    }

    #[test]
    fn parse_job_getinfo() {
        let cmd = parse_command("cmd=job-getinfo;key=universeSize;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::JobGetInfo {
                key: "universeSize".to_string(),
            }
        );
    }

    #[test]
    fn parse_unknown_command() {
        let result = parse_command("cmd=bogus;\n");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown"));
    }

    #[test]
    fn parse_missing_cmd() {
        let result = parse_command("key=value;\n");
        assert!(result.is_err());
    }

    #[test]
    fn format_fullinit_response() {
        let resp = Pmi2Response::FullInitResp {
            rank: 3,
            size: 16,
            appnum: 0,
            pmi_version: 2,
            pmi_subversion: 0,
            spawner_jobid: "lattice-abc123".to_string(),
            rc: 0,
        };
        let wire = format_response(&resp);
        assert!(wire.contains("cmd=fullinit-response"));
        assert!(wire.contains("rank=3"));
        assert!(wire.contains("size=16"));
        assert!(wire.ends_with('\n'));
    }

    #[test]
    fn format_kvsput_response() {
        let resp = Pmi2Response::KvsPutResp { rc: 0 };
        let wire = format_response(&resp);
        assert_eq!(wire, "cmd=kvsput-response;rc=0;\n");
    }

    #[test]
    fn format_kvsget_response() {
        let resp = Pmi2Response::KvsGetResp {
            key: "k".to_string(),
            value: "v".to_string(),
            found: true,
            rc: 0,
        };
        let wire = format_response(&resp);
        assert!(wire.contains("found=true"));
        assert!(wire.contains("value=v"));
    }

    #[test]
    fn format_kvsfence_response() {
        let resp = Pmi2Response::KvsFenceResp { rc: 0 };
        let wire = format_response(&resp);
        assert_eq!(wire, "cmd=kvsfence-response;rc=0;\n");
    }

    #[test]
    fn roundtrip_parse_fields() {
        let fields = parse_fields("cmd=kvsput;key=a;value=b;");
        assert_eq!(fields.get("cmd").unwrap(), "kvsput");
        assert_eq!(fields.get("key").unwrap(), "a");
        assert_eq!(fields.get("value").unwrap(), "b");
    }

    #[test]
    fn parse_empty_line() {
        let result = parse_command("");
        assert!(result.is_err(), "empty line should fail due to missing cmd");
    }

    #[test]
    fn parse_no_semicolons() {
        // "cmd=fullinit" with no trailing semicolon — parse_fields splits on ';',
        // but the whole string is a single segment "cmd=fullinit" which still parses.
        let result = parse_command("cmd=fullinit");
        assert_eq!(
            result.unwrap(),
            Pmi2Command::FullInit {
                pmi_version: 2,
                pmi_subversion: 0,
            }
        );
    }

    #[test]
    fn parse_duplicate_keys() {
        // Duplicate keys: HashMap insert overwrites, so last value wins.
        let fields = parse_fields("cmd=fullinit;cmd=abort;");
        // Either "fullinit" or "abort" depending on HashMap iteration, but
        // since we insert sequentially the last insert wins.
        assert_eq!(fields.get("cmd").unwrap(), "abort");

        // So parsing "cmd=fullinit;cmd=abort;" gives Abort command
        let cmd = parse_command("cmd=fullinit;cmd=abort;").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::Abort {
                message: String::new(),
            }
        );
    }

    #[test]
    fn parse_special_chars_in_value() {
        // "value=http://10.0.0.1:5000" — the colon and slashes are fine,
        // but extra semicolons would split the value.
        // With "cmd=kvsput;key=addr;value=http://10.0.0.1:5000;" the value
        // is "http://10.0.0.1:5000" because split_once('=') takes everything
        // after the first '='.
        let cmd = parse_command("cmd=kvsput;key=addr;value=http://10.0.0.1:5000;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::KvsPut {
                key: "addr".into(),
                value: "http://10.0.0.1:5000".into(),
            }
        );
    }

    #[test]
    fn parse_special_chars_semicolon_in_value_truncates() {
        // If the value itself contains a semicolon, it gets split.
        // "value=a;b" becomes field "value"="a" and leftover "b" (no '=' so ignored).
        let cmd = parse_command("cmd=kvsput;key=k;value=a;b;\n").unwrap();
        assert_eq!(
            cmd,
            Pmi2Command::KvsPut {
                key: "k".into(),
                value: "a".into(), // truncated at semicolon
            }
        );
    }

    #[test]
    fn format_and_reparse_fullinit() {
        let resp = Pmi2Response::FullInitResp {
            rank: 5,
            size: 32,
            appnum: 0,
            pmi_version: 2,
            pmi_subversion: 0,
            spawner_jobid: "lattice-abc".into(),
            rc: 0,
        };
        let wire = format_response(&resp);
        // Verify the wire format contains all expected fields
        assert!(wire.contains("cmd=fullinit-response"));
        assert!(wire.contains("rc=0"));
        assert!(wire.contains("rank=5"));
        assert!(wire.contains("size=32"));
        assert!(wire.contains("appnum=0"));
        assert!(wire.contains("pmi_version=2"));
        assert!(wire.contains("pmi_subversion=0"));
        assert!(wire.contains("spawner-jobid=lattice-abc"));
        assert!(wire.ends_with('\n'));

        // Parse back with parse_fields and verify key fields
        let fields = parse_fields(wire.trim_end_matches('\n'));
        assert_eq!(fields.get("cmd").unwrap(), "fullinit-response");
        assert_eq!(fields.get("rank").unwrap(), "5");
        assert_eq!(fields.get("size").unwrap(), "32");
    }
}
