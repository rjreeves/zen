use crate::ast::FunctionCall;
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginResult, ZenPlugin};
use crate::runtime::values::Value;

pub struct TimePlugin;

static TIME_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "time",
        summary: "Returns or transforms UTC time values.",
        usage: "time [now|unix|millis|format|stamp|since|until|parse|freeze]",
        examples: &["time", "time unix", "time format \"%Y-%m-%d\""],
    },
    CommandDoc {
        command: "time.now",
        summary: "Returns the current UTC timestamp in RFC3339 form.",
        usage: "time.now",
        examples: &["time.now"],
    },
    CommandDoc {
        command: "time.timestamp",
        summary: "Alias for time.now.",
        usage: "time.timestamp",
        examples: &["time.timestamp"],
    },
    CommandDoc {
        command: "time.unix",
        summary: "Returns the current Unix timestamp in seconds.",
        usage: "time.unix",
        examples: &["time.unix"],
    },
    CommandDoc {
        command: "time.millis",
        summary: "Returns the current Unix timestamp in milliseconds.",
        usage: "time.millis",
        examples: &["time.millis"],
    },
    CommandDoc {
        command: "time.format",
        summary: "Formats the current time or pipeline timestamp for local display.",
        usage: "time.format [pattern]",
        examples: &[
            "time.format \"%Y-%m-%d\"",
            "time.now | time.format \"%I:%M:%S %p\"",
        ],
    },
    CommandDoc {
        command: "time.stamp",
        summary: "Adds a UTC timestamp field to the pipeline value.",
        usage: "time.stamp [field]",
        examples: &["time.stamp", "time.stamp created_at"],
    },
    CommandDoc {
        command: "time.since",
        summary: "Returns a duration summary from a past time to now.",
        usage: "time.since <time>",
        examples: &[
            "time.since yesterday",
            "time.since \"2026-05-14T12:34:56Z\"",
        ],
    },
    CommandDoc {
        command: "time.until",
        summary: "Returns a duration summary from now until a future time.",
        usage: "time.until <time>",
        examples: &["time.until tomorrow", "time.until \"in 2 hours\""],
    },
    CommandDoc {
        command: "time.parse",
        summary: "Parses a time phrase or timestamp into UTC RFC3339 form.",
        usage: "time.parse <phrase>",
        examples: &["time.parse yesterday", "time.parse \"in 2 hours\""],
    },
    CommandDoc {
        command: "time.freeze",
        summary: "Freezes or clears the executor clock for deterministic time workflows.",
        usage: "time.freeze <rfc3339|clear>",
        examples: &["time.freeze \"2026-05-14T12:34:56Z\"", "time.freeze clear"],
    },
    CommandDoc {
        command: "time.local",
        summary: "Returns or formats local time.",
        usage: "time.local [now|format|stamp]",
        examples: &["time.local", "time.local.format \"%Y-%m-%d %H:%M\""],
    },
    CommandDoc {
        command: "time.local.now",
        summary: "Returns the current local timestamp.",
        usage: "time.local.now",
        examples: &["time.local.now"],
    },
    CommandDoc {
        command: "time.local.format",
        summary: "Formats the current time or pipeline timestamp in local time.",
        usage: "time.local.format [pattern]",
        examples: &["time.local.format \"%Y-%m-%d %H:%M\""],
    },
    CommandDoc {
        command: "time.local.stamp",
        summary: "Adds a local timestamp field to the pipeline value.",
        usage: "time.local.stamp [field]",
        examples: &["time.local.stamp", "time.local.stamp local_time"],
    },
    CommandDoc {
        command: "measure",
        summary: "Runs a command and returns timing metadata with its result.",
        usage: "measure <command>",
        examples: &["measure sleep 20ms", "measure exec pg_dump --version"],
    },
    CommandDoc {
        command: "benchmark",
        summary: "Runs a command multiple times and returns timing summary statistics.",
        usage: "benchmark <runs> <command>",
        examples: &["benchmark 5 sleep 20ms"],
    },
    CommandDoc {
        command: "sleep",
        summary: "Waits for a duration and returns the pipeline input.",
        usage: "sleep <duration> [jitter <duration>]",
        examples: &["sleep 500ms", "sleep 1s jitter 100ms"],
    },
];

impl ZenPlugin for TimePlugin {
    fn name(&self) -> &'static str {
        "time"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "time",
            "time.now",
            "time.timestamp",
            "time.unix",
            "time.millis",
            "time.format",
            "time.stamp",
            "time.since",
            "time.until",
            "time.parse",
            "time.freeze",
            "time.local",
            "time.local.now",
            "time.local.format",
            "time.local.stamp",
            "measure",
            "benchmark",
            "sleep",
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        TIME_DOCS
    }

    fn call(
        &self,
        executor: &mut Executor,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String> {
        let result = match call.name.join(".").as_str() {
            "time" => executor.time_builtin(call.args.clone(), input.clone()),
            "time.now" => executor.time_builtin_with_mode("now", call.args.clone(), input.clone()),
            "time.timestamp" => {
                executor.time_builtin_with_mode("timestamp", call.args.clone(), input.clone())
            }
            "time.unix" => {
                executor.time_builtin_with_mode("unix", call.args.clone(), input.clone())
            }
            "time.millis" => {
                executor.time_builtin_with_mode("millis", call.args.clone(), input.clone())
            }
            "time.format" => {
                executor.time_builtin_with_mode("format", call.args.clone(), input.clone())
            }
            "time.stamp" => {
                executor.time_builtin_with_mode("stamp", call.args.clone(), input.clone())
            }
            "time.since" => {
                executor.time_builtin_with_mode("since", call.args.clone(), input.clone())
            }
            "time.until" => {
                executor.time_builtin_with_mode("until", call.args.clone(), input.clone())
            }
            "time.parse" => {
                executor.time_builtin_with_mode("parse", call.args.clone(), input.clone())
            }
            "time.freeze" => executor.time_freeze(call.args.clone(), input.clone()),
            "time.local" => executor.time_local(call.args.clone(), input.clone()),
            "time.local.now" => {
                executor.time_local_with_mode("now", call.args.clone(), input.clone())
            }
            "time.local.format" => {
                executor.time_local_with_mode("format", call.args.clone(), input.clone())
            }
            "time.local.stamp" => {
                executor.time_local_with_mode("stamp", call.args.clone(), input.clone())
            }
            "measure" => executor.time_measure(call.args.clone(), input.clone()),
            "benchmark" => executor.time_benchmark(call.args.clone(), input.clone()),
            "sleep" => executor.time_sleep(call.args.clone(), input.clone()),
            _ => return Ok(PluginResult::unhandled()),
        }?;

        Ok(PluginResult::handled(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FunctionCall;
    use crate::permissions::PermissionSet;

    #[test]
    fn time_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = TimePlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_time".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }
}
