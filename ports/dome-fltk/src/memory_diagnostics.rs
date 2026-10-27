// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in local memory snapshots, aggregated without document URLs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use servo_base::generic_channel::GenericCallback;

pub struct Capture {
    path: Option<PathBuf>,
    delay: Duration,
    interval: Duration,
    count: u32,
    completed: u32,
}

fn milliseconds(name: &str, fallback: u64) -> Duration {
    Duration::from_millis(std::env::var(name).ok()
        .and_then(|v| v.parse::<u64>().ok()).unwrap_or(fallback).clamp(1_000, 120_000))
}

impl Capture {
    pub fn from_environment() -> Self {
        Self {
            path: std::env::var_os("DOME_BROWSER_MEMORY_CAPTURE").map(PathBuf::from),
            delay: milliseconds("DOME_BROWSER_MEMORY_CAPTURE_DELAY_MS", 15_000),
            interval: milliseconds("DOME_BROWSER_MEMORY_CAPTURE_INTERVAL_MS", 60_000),
            count: std::env::var("DOME_BROWSER_MEMORY_CAPTURE_COUNT").ok()
                .and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).clamp(1, 24),
            completed: 0,
        }
    }

    pub fn update(&mut self, servo: &servo::Servo, elapsed: Duration) {
        let Some(path) = self.path.as_ref() else { return; };
        if self.completed >= self.count || elapsed < self.delay + self.interval * self.completed {
            return;
        }
        let sequence = self.completed;
        self.completed += 1;
        let path = if sequence == 0 { path.clone() } else { path.with_extension(format!("{sequence}.json")) };
        match GenericCallback::new(move |report| {
            let report = match report {
                Ok(report) => report,
                Err(_) => { eprintln!("MEMORY_CAPTURE: report unavailable"); return; },
            };
            match serde_json::to_value(report).map(aggregate) {
                Ok(mut report) => {
                    report["elapsed_ms"] = json!(elapsed.as_millis());
                    report["sequence"] = json!(sequence);
                    if let Err(error) = crate::storage::write(&path, &report.to_string()) {
                        eprintln!("MEMORY_CAPTURE: {error}");
                    }
                },
                Err(error) => eprintln!("MEMORY_CAPTURE: {error}"),
            }
        }) {
            Ok(callback) => servo.create_memory_report(callback),
            Err(error) => eprintln!("MEMORY_CAPTURE: {error:?}"),
        }
    }
}

fn aggregate(report: Value) -> Value {
    let mut totals: BTreeMap<(String, Vec<String>), (u64, u64)> = BTreeMap::new();
    for process in report["results"].as_array().into_iter().flatten() {
        for row in process["reports"].as_array().into_iter().flatten() {
            let mut objects = 0;
            let path: Vec<_> = row["path"].as_array().into_iter().flatten().map(|part| {
                let text = part.as_str().unwrap_or_default();
                if text.starts_with("url(") || text.contains("://") {
                    return if text.rsplit_once(')').is_some_and(|(_, suffix)| suffix.starts_with("-zombie-")) {
                        "document-zombie".into()
                    } else { "document".into() };
                }
                if let Some((name, count)) = text.strip_suffix(']').and_then(|s| s.rsplit_once(" [")) {
                    if let Ok(count) = count.parse::<u64>() {
                        objects += count;
                        return name.to_owned();
                    }
                }
                text.to_owned()
            }).collect();
            let key = (row["kind"].as_str().unwrap_or_default().to_owned(), path);
            let total = totals.entry(key).or_default();
            total.0 = total.0.saturating_add(row["size"].as_u64().unwrap_or(0));
            total.1 = total.1.saturating_add(objects);
        }
    }
    json!({"scope":"Aggregated memory categories and DOM counts; document URLs omitted", "reports":totals.into_iter().map(|((kind,path),(bytes,objects))|json!({"kind":kind,"path":path,"bytes":bytes,"objects":objects})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_interfaces_without_document_urls() {
        let result = aggregate(json!({"results":[{"reports":[
            {"path":["url(https://private.example/?secret=one)","js","dom","out-of-tree","url(https://private.example/)-1","HTMLSpanElement [9]"],"kind":"ExplicitJemallocHeapSize","size":100},
            {"path":["url(https://other.example/)","js","dom","out-of-tree","url(https://other.example/)-1","HTMLSpanElement [3]"],"kind":"ExplicitJemallocHeapSize","size":70}
        ]}]}));
        assert_eq!(result["reports"].as_array().unwrap().len(), 1);
        assert_eq!(result["reports"][0]["bytes"], 170);
        assert_eq!(result["reports"][0]["objects"], 12);
        let serialized = result.to_string();
        assert!(!serialized.contains("private.example") && !serialized.contains("secret") && !serialized.contains("other.example"));
    }

    #[test]
    fn distinguishes_zombie_dom_without_retaining_its_url() {
        let result = aggregate(json!({"results":[{"reports":[
            {"path":["url(about:blank)","js","dom","out-of-tree","url(https://private.example/path))-zombie-2","Window [1]"],"kind":"ExplicitJemallocHeapSize","size":80}
        ]}]}));
        assert_eq!(result["reports"][0]["objects"], 1);
        assert!(result.to_string().contains("document-zombie"));
        assert!(!result.to_string().contains("private.example"));
    }
}
