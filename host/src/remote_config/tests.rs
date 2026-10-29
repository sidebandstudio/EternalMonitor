use super::*;
use serde_json::json;

/// The example manifest shared with the iPad's and check-versions' tests.
const EXAMPLE: &str = include_str!("../../../proto/testdata/latest_remote_config.json");
/// 2026-11-02T00:00:00Z, the example's rollout start.
const START: u64 = 1_793_577_600;

fn temp_file(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "eternal-remote-config-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join(FILE_NAME)
}

/// A laptop with Intel and NVIDIA graphics, as the second screen's
/// fingerprint lists it.
fn hybrid() -> Vec<String> {
    vec![
        "Intel(R) UHD Graphics 31.0.101.4502".to_string(),
        "NVIDIA GeForce RTX 3050 Laptop GPU 32.0.15.6094".to_string(),
        "Microsoft Basic Render Driver 10.0.26100.1".to_string(),
    ]
}

fn ids(count: u32) -> impl Iterator<Item = String> {
    (0..count).map(|i| format!("{:032x}", u128::from(i) * 0x9e37_79b9))
}

#[test]
fn utc_times_parse_only_in_the_one_form() {
    assert_eq!(parse_utc("2026-11-02T00:00:00Z"), Some(START));
    assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_utc("2028-02-29T12:34:56Z"), Some(1_835_440_496));
    for bad in [
        "",
        "2026-11-02",
        "2026-11-02T00:00:00",
        "2026-11-02T00:00:00+00:00",
        "2026-11-02T00:00:00.5Z",
        "2026-11-02 00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2027-02-29T00:00:00Z",
        "2026-11-02T24:00:00Z",
        "1969-12-31T23:59:59Z",
        "+026-11-02T00:00:00Z",
        "２026-11-02T00:00:00Z",
    ] {
        assert_eq!(parse_utc(bad), None, "{bad}");
    }
}

#[test]
fn the_rollout_bucket_is_stable_and_shared_with_the_ipad() {
    // UpdateCheckerTests.swift asserts the same two values.
    assert_eq!(
        bucket("00000000000000000000000000000000", "0.3.4"),
        5_056_594_762_205_651_656
    );
    assert_eq!(
        bucket("0123456789abcdef0123456789abcdef", "0.3.4"),
        4_748_426_906_821_000_708
    );
    assert_ne!(
        bucket("0123456789abcdef0123456789abcdef", "0.3.5"),
        bucket("0123456789abcdef0123456789abcdef", "0.3.4"),
        "each release ramps another set of installs first"
    );
}

#[test]
fn the_rollout_bucket_is_uniform() {
    let mut deciles = [0u32; 10];
    for id in ids(10_000) {
        deciles[((u128::from(bucket(&id, "0.3.4")) * 10) >> 64) as usize] += 1;
    }
    for (decile, count) in deciles.iter().enumerate() {
        assert!((880..=1120).contains(count), "decile {decile}: {count}");
    }
}

#[test]
fn a_rollout_ramps_from_few_to_everyone() {
    let rollout = Rollout::parse(&json!({ "start": "2026-11-02T00:00:00Z", "days": 7 })).unwrap();
    let shown = |now: u64| {
        ids(10_000)
            .filter(|id| rollout.shows(id, "0.3.4", now))
            .count()
    };
    assert_eq!(shown(START - 1), 0, "before the start");
    assert!(
        shown(START + 60) < 20,
        "the first minute: {}",
        shown(START + 60)
    );
    let first_day = shown(START + DAY);
    assert!((1_250..=1_600).contains(&first_day), "day 1: {first_day}");
    let half = shown(START + 7 * DAY / 2);
    assert!((4_700..=5_300).contains(&half), "half way: {half}");
    assert_eq!(shown(START + 7 * DAY), 10_000, "the last day");
    // An install that sees it keeps seeing it.
    for id in ids(500) {
        let first = (0..=7 * 24).find(|h| rollout.shows(&id, "0.3.4", START + h * 3600));
        let first = first.unwrap();
        assert!((first..=7 * 24).all(|h| rollout.shows(&id, "0.3.4", START + h * 3600)));
    }
    // No ramp: everyone at the start.
    let at_once = Rollout::parse(&json!({ "start": "2026-11-02T00:00:00Z", "days": 0 })).unwrap();
    assert!(ids(100).all(|id| at_once.shows(&id, "0.3.4", START)));
    assert!(ids(100).all(|id| !at_once.shows(&id, "0.3.4", START - 1)));
    // A start far ahead is a typo: nobody waits for it.
    let typo = Rollout::parse(&json!({ "start": "2099-01-01T00:00:00Z", "days": 7 })).unwrap();
    assert!(ids(100).all(|id| typo.shows(&id, "0.3.4", START)));
}

/// Review round 2: values that could only come from a tampered cache
/// overflowed. Any start or ramp shows rather than panicking or hiding.
#[test]
fn extreme_rollouts_never_overflow_or_hide_for_long() {
    for (start, days) in [
        (u64::MAX, 7),
        (u64::MAX, u64::MAX),
        (0, u64::MAX),
        (START, 365),
    ] {
        let rollout = Rollout { start, days };
        for now in [0, START, u64::MAX] {
            let shows = rollout.shows("00000000000000000000000000000000", "0.3.4", now);
            if days > MAX_ROLLOUT_DAYS || start > now.saturating_add(MAX_ROLLOUT_DAYS * DAY) {
                assert!(shows, "{start} {days} {now}");
            }
        }
    }
}

#[test]
fn a_bad_rollout_is_no_rollout() {
    for bad in [
        json!(null),
        json!("2026-11-02T00:00:00Z"),
        json!({ "start": "2026-11-02T00:00:00Z" }),
        json!({ "days": 7 }),
        json!({ "start": "2026-11-02T00:00:00Z", "days": 15 }),
        json!({ "start": "2026-11-02T00:00:00Z", "days": -1 }),
        json!({ "start": "2026-11-02T00:00:00Z", "days": 2.5 }),
        json!({ "start": "2026-11-02T00:00:00Z", "days": "7" }),
        json!({ "start": "next week", "days": 7 }),
        json!({ "start": 1_793_577_600, "days": 7 }),
    ] {
        assert_eq!(Rollout::parse(&bad), None, "{bad}");
    }
}

#[test]
fn the_example_manifest_parses() {
    let manifest: Value = serde_json::from_str(EXAMPLE).unwrap();
    let windows = &manifest["windows"];
    assert_eq!(
        Rollout::parse(&windows["rollout"]),
        Some(Rollout {
            start: START,
            days: 7
        })
    );
    let config = parse_config(Some(&windows["config"])).unwrap();
    assert_eq!(config.attach_timeout_s, Some(45));
    assert_eq!(config.rules.len(), 2);
    assert!(config.rules[0].matches(&hybrid(), "25.7.23", "26100.2894"));
    assert!(!config.rules[1].matches(&hybrid(), "25.7.23", "26100.2894"));
}

#[test]
fn missing_garbage_or_newer_config_is_the_defaults() {
    let cases = [
        (None, "missing"),
        (Some(json!(null)), "missing"),
        (Some(json!("schema 1")), "invalid"),
        (Some(json!([])), "invalid"),
        (Some(json!({})), "invalid"),
        (Some(json!({ "schema": "1" })), "invalid"),
        (Some(json!({ "schema": 0 })), "invalid"),
        (
            Some(json!({ "schema": 2, "attach_timeout_s": 45 })),
            "schema",
        ),
    ];
    for (value, reason) in cases {
        assert_eq!(parse_config(value.as_ref()), Err(reason), "{value:?}");
    }
}

#[test]
fn the_attach_wait_is_clamped_to_a_safe_range() {
    for (value, applied) in [
        (json!(30), Some(30)),
        (json!(45), Some(45)),
        (json!(60), Some(60)),
        // Shorter than today's 30 s would fail PCs that attach late.
        (json!(20), None),
        (json!(29), None),
        (json!(19), None),
        (json!(61), None),
        (json!(0), None),
        (json!(-30), None),
        (json!(45.5), None),
        (json!("45"), None),
        (json!(u64::MAX), None),
    ] {
        let config =
            parse_config(Some(&json!({ "schema": 1, "attach_timeout_s": value }))).unwrap();
        assert_eq!(config.attach_timeout_s, applied, "{value}");
    }
    let applied = Applied {
        rollout: None,
        config: parse_config(Some(&json!({ "schema": 1, "attach_timeout_s": 45 }))),
    };
    assert_eq!(applied.attach_timeout(), Some(Duration::from_secs(45)));
    assert_eq!(Applied::defaults("missing").attach_timeout(), None);
}

fn rule(value: Value) -> Option<Rule> {
    Rule::parse(&value)
}

#[test]
fn a_rule_matches_only_its_hardware() {
    let rule = rule(json!({
        "gpus": ["nvidia", "intel"],
        "gpu_driver": { "gpu": "nvidia", "min": "32.0.15.6000", "max": "32.0.15.6099" },
        "display_driver": { "min": "25.7.0", "max": "25.7.99" },
        "windows": { "min": "26100.0", "max": "26100.2999" }
    }))
    .unwrap();
    assert!(rule.matches(&hybrid(), "25.7.23", "26100.2894"));
    // Another driver, VDD, Windows build or set of cards.
    assert!(!rule.matches(&hybrid(), "25.7.23", "26100.3194"));
    assert!(!rule.matches(&hybrid(), "25.11.3", "26100.2894"));
    assert!(!rule.matches(&hybrid(), "unknown", "26100.2894"));
    let mut newer = hybrid();
    newer[1] = "NVIDIA GeForce RTX 3050 Laptop GPU 32.0.15.6614".to_string();
    assert!(!rule.matches(&newer, "25.7.23", "26100.2894"));
    let mut unknown = hybrid();
    unknown[1] = "NVIDIA GeForce RTX 3050 Laptop GPU unknown".to_string();
    assert!(!rule.matches(&unknown, "25.7.23", "26100.2894"));
    let intel_only = vec![hybrid()[0].clone()];
    assert!(!rule.matches(&intel_only, "25.7.23", "26100.2894"));
    let mut with_amd = hybrid();
    with_amd.push("AMD Radeon RX 6600 31.0.21001.45002".to_string());
    assert!(!rule.matches(&with_amd, "25.7.23", "26100.2894"));
    let mut with_parsec = hybrid();
    with_parsec.push("Parsec Virtual Display Adapter 0.45.0.0".to_string());
    assert!(!rule.matches(&with_parsec, "25.7.23", "26100.2894"));
    assert!(!rule.matches(&[], "25.7.23", "26100.2894"));
}

/// A rule that passes, to break one way at a time.
fn narrow_rule() -> Value {
    json!({
        "gpus": ["intel", "nvidia"],
        "gpu_driver": { "gpu": "nvidia", "min": "32.0.15.6000", "max": "32.0.15.6099" },
        "display_driver": { "min": "25.7.0", "max": "25.7.99" },
        "windows": { "min": "26100.0", "max": "26100.2999" }
    })
}

#[test]
fn broad_or_unknown_rules_are_ignored() {
    assert!(rule(narrow_rule()).is_some());
    let with = |change: &dyn Fn(&mut Value)| {
        let mut value = narrow_rule();
        change(&mut value);
        value
    };
    let without = |keys: &'static [&'static str]| {
        with(&move |v| {
            for key in keys {
                v.as_object_mut().unwrap().remove(*key);
            }
        })
    };
    // Exact releases pass: one driver, one VDD version, one Windows update.
    let exact = with(&|v| {
        v["gpu_driver"] = json!({ "gpu": "nvidia", "min": "32.0.15.6094", "max": "32.0.15.6094" });
        v["display_driver"] = json!({ "min": "25.7.23", "max": "25.7.23" });
        v["windows"] = json!({ "min": "26100.2894", "max": "26100.2894" });
    });
    assert!(rule(exact).is_some());
    assert!(rule(without(&["windows"])).is_some());
    assert!(rule(without(&["display_driver"])).is_some());
    let bad = [
        // The review's soft kill switch: every NVIDIA-only PC.
        json!({
            "gpus": ["nvidia"],
            "gpu_driver": { "gpu": "nvidia", "min": "32.0", "max": "32.4294967295" },
            "windows": { "min": "26100.0", "max": "26100.99999" }
        }),
        // Every PC on Windows 11 24H2 with these cards: no driver release.
        without(&["gpu_driver"]),
        json!({ "gpus": ["intel", "nvidia"], "windows": { "min": "26100.0", "max": "26100.99999" } }),
        // A driver release and nothing else.
        without(&["display_driver", "windows"]),
        // The vendor set alone, or nothing at all.
        json!({ "gpus": ["intel", "nvidia"] }),
        without(&["gpus"]),
        with(&|v| v["gpus"] = json!([])),
        with(&|v| v["gpus"] = json!(["intel", "nvidia", "nvidia"])),
        with(&|v| v["gpus"] = json!(["Intel", "nvidia"])),
        with(&|v| v["gpus"] = json!(["other", "nvidia"])),
        with(&|v| v["gpus"] = json!("nvidia")),
        // Graphics driver ranges wider than one release.
        with(&|v| v["gpu_driver"]["max"] = json!("32.0.15.6100")),
        with(&|v| v["gpu_driver"]["min"] = json!("32.0.15.0")),
        with(&|v| v["gpu_driver"]["max"] = json!("32.0.15.9999")),
        with(&|v| v["gpu_driver"]["max"] = json!("32.0.16.6000")),
        with(&|v| {
            v["gpu_driver"]["min"] = json!("32.0.15");
            v["gpu_driver"]["max"] = json!("32.0.15");
        }),
        with(&|v| v["gpu_driver"]["min"] = json!("32.0.15.6100")),
        with(&|v| v["gpu_driver"]["gpu"] = json!("amd")),
        with(&|v| v["gpu_driver"] = json!({ "gpu": "nvidia", "min": "32.0.15.6000" })),
        // A VDD range across minor versions, or a bare major.
        with(&|v| v["display_driver"] = json!({ "min": "25.7.0", "max": "25.8.0" })),
        with(&|v| v["display_driver"] = json!({ "min": "25", "max": "25" })),
        with(&|v| v["display_driver"] = json!({ "min": "25.7", "max": "25.7.99" })),
        with(&|v| v["display_driver"] = json!({ "min": "0", "max": "4294967295" })),
        // Windows: another build, or a build without its update number.
        with(&|v| v["windows"] = json!({ "min": "22631.0", "max": "26100.9999" })),
        with(&|v| v["windows"] = json!({ "min": "26100", "max": "26100" })),
        with(&|v| v["windows"] = json!({ "min": "26100.0.0", "max": "26100.0.9" })),
        // Reversed or malformed.
        with(&|v| v["display_driver"] = json!({ "min": "25.7.99", "max": "25.7.0" })),
        with(&|v| v["display_driver"] = json!({ "min": "25.7.x", "max": "25.7.9" })),
        with(&|v| v["display_driver"] = json!({ "min": "25.1.2.3.4", "max": "25.1.2.3.5" })),
        with(&|v| v["display_driver"] = json!({ "min": "", "max": "25.7.9" })),
        with(&|v| v["display_driver"] = json!({ "min": 25, "max": 26 })),
        // A constraint this app doesn't know would be dropped, widening the
        // rule: the whole rule goes instead.
        with(&|v| v["cpu"] = json!("arm64")),
        with(&|v| v["display_driver"]["beta"] = json!(true)),
        json!(null),
        json!([]),
    ];
    for bad in bad {
        assert_eq!(rule(bad.clone()), None, "{bad}");
    }
}

#[test]
fn too_many_rules_apply_none() {
    let one = narrow_rule();
    let config = |count: usize| {
        parse_config(Some(&json!({
            "schema": 1,
            "second_screen_rules": vec![one.clone(); count]
        })))
        .unwrap()
    };
    assert_eq!(config(MAX_RULES).rules.len(), MAX_RULES);
    assert!(config(MAX_RULES + 1).rules.is_empty());
    // A bad rule among good ones drops only itself.
    let mixed = parse_config(Some(&json!({
        "schema": 1,
        "second_screen_rules": [{ "gpus": ["intel"] }, one, "garbage"]
    })))
    .unwrap();
    assert_eq!(mixed.rules.len(), 1);
    assert!(
        parse_config(Some(&json!({ "schema": 1, "second_screen_rules": one })))
            .unwrap()
            .rules
            .is_empty()
    );
}

#[test]
fn the_saved_config_applies_for_two_weeks() {
    let path = temp_file("saved");
    assert_eq!(read(&path, START), Applied::defaults("missing"));
    write(&path, EXAMPLE.as_bytes(), START).unwrap();
    let applied = read(&path, START + DAY);
    assert_eq!(applied.attach_timeout(), Some(Duration::from_secs(45)));
    assert!(applied.second_screen_rule(&hybrid(), "25.7.23", "26100.2894"));
    assert!(!applied.second_screen_rule(&hybrid(), "25.11.3", "26100.2894"));
    assert_eq!(
        applied.summary(Some(true)),
        "remote_config=applied rollout=7d ss_rule=match attach_s=45"
    );
    assert_eq!(read(&path, START + MAX_AGE), applied);
    assert_eq!(read(&path, START + MAX_AGE + 1), Applied::defaults("stale"));
    assert_eq!(
        read(&path, START - 1),
        Applied::defaults("stale"),
        "clock set back"
    );

    // The next manifest has no config: defaults, said once.
    let plain = include_str!("../../../docs/latest.json");
    write(&path, plain.as_bytes(), START).unwrap();
    let applied = read(&path, START);
    assert_eq!(applied, Applied::defaults("missing"));
    assert_eq!(
        applied.summary(None),
        "remote_config=defaults reason=missing rollout=none"
    );
    assert_eq!(applied.attach_timeout(), None);
    assert!(!applied.second_screen_rule(&hybrid(), "25.7.23", "26100.2894"));

    // A captive portal's page, or a manifest from a newer schema.
    for body in [
        b"<html>Sign in to Wi-Fi</html>".to_vec(),
        json!({ "schema": 2, "windows": { "config": { "schema": 1, "attach_timeout_s": 45 } } })
            .to_string()
            .into_bytes(),
    ] {
        write(&path, &body, START).unwrap();
        assert_eq!(read(&path, START), Applied::defaults("missing"));
    }
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn an_unreadable_saved_config_is_the_defaults() {
    let path = temp_file("unreadable");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for content in [
        String::new(),
        "{".to_string(),
        "[]".to_string(),
        json!({ "format": 2, "fetched_at": START, "config": { "schema": 1 } }).to_string(),
        json!({ "format": 1, "fetched_at": "today" }).to_string(),
    ] {
        std::fs::write(&path, &content).unwrap();
        assert_eq!(
            read(&path, START),
            Applied::defaults("unreadable"),
            "{content}"
        );
    }
    std::fs::write(
        &path,
        json!({ "format": 1, "fetched_at": START, "config": { "schema": 3 } }).to_string(),
    )
    .unwrap();
    let applied = read(&path, START);
    assert_eq!(applied, Applied::defaults("schema"));
    assert_eq!(
        applied.summary(None),
        "remote_config=defaults reason=schema rollout=none"
    );
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn a_config_without_values_logs_its_defaults() {
    let applied = Applied {
        rollout: None,
        config: parse_config(Some(&json!({ "schema": 1, "attach_timeout_s": 600 }))),
    };
    assert_eq!(
        applied.summary(Some(false)),
        "remote_config=applied rollout=none ss_rule=none attach_s=default"
    );
    assert_eq!(
        applied.summary(None),
        "remote_config=applied rollout=none ss_rule=n/a attach_s=default"
    );
}

fn renderer_rule(value: Value) -> Option<RendererRule> {
    RendererRule::parse(&value)
}

const NVIDIA: u32 = 0x10de;
const AMD: u32 = 0x1002;
const INTEL: u32 = 0x8086;

#[test]
fn renderer_rules_parse_and_match_by_vendor_and_driver() {
    let rule = renderer_rule(json!({
        "vendor": "intel",
        "driver": { "min": "31.0.101.0", "max": "31.0.101.65535" },
        "backend": "dx12"
    }))
    .unwrap();
    assert_eq!(rule.backend, RendererBackend::Dx12);
    assert_eq!(rule.vendor(), "intel");
    assert!(rule.matches(INTEL, "31.0.101.2111"));
    assert!(rule.matches(INTEL, "31.0.101.0"));
    assert!(rule.matches(INTEL, "31.0.101.65535"));
    assert!(!rule.matches(INTEL, "31.0.102.0"));
    assert!(!rule.matches(INTEL, "32.0.101.6078"));
    assert!(!rule.matches(INTEL, "30.0.101.1994"));
    assert!(!rule.matches(NVIDIA, "31.0.101.2111"));
    // wgpu's driver text, when DXGI didn't list the GPU, is no version.
    assert!(!rule.matches(INTEL, "Intel Corporation Intel driver"));
    assert!(!rule.matches(INTEL, "31.0.101"));
    assert!(!rule.matches(INTEL, ""));

    // One end only, or no range: every driver of the vendor.
    let from = renderer_rule(json!({
        "vendor": "nvidia", "driver": { "min": "32.0.15.6000" }, "backend": "vulkan"
    }))
    .unwrap();
    assert!(from.matches(NVIDIA, "32.0.15.6000"));
    assert!(from.matches(NVIDIA, "33.0.15.1"));
    assert!(!from.matches(NVIDIA, "32.0.15.5999"));
    let until = renderer_rule(json!({
        "vendor": "amd", "driver": { "max": "31.0.21001.45099" }, "backend": "warp"
    }))
    .unwrap();
    assert_eq!(until.backend, RendererBackend::Warp);
    assert!(until.matches(AMD, "31.0.21001.45099"));
    assert!(until.matches(AMD, "25.0.0.0"));
    assert!(!until.matches(AMD, "31.0.21001.45100"));
    let any = renderer_rule(json!({ "vendor": "amd", "backend": "dx12" })).unwrap();
    assert!(any.matches(AMD, "32.0.21025.1024"));
    assert!(any.matches(AMD, "AMD proprietary driver"));
    assert!(!any.matches(INTEL, "32.0.21025.1024"));
    assert!(!any.matches(0x1414, "10.0.26100.1"));
}

#[test]
fn a_renderer_rule_with_anything_unknown_is_ignored() {
    let good = json!({
        "vendor": "intel",
        "driver": { "min": "31.0.101.0", "max": "31.0.101.65535" },
        "backend": "dx12"
    });
    assert!(renderer_rule(good.clone()).is_some());
    let with = |change: &dyn Fn(&mut Value)| {
        let mut value = good.clone();
        change(&mut value);
        value
    };
    let bad = [
        // Vendors second_screen_rules knows but the renderer doesn't, other
        // spellings, ids.
        with(&|v| v["vendor"] = json!("qualcomm")),
        with(&|v| v["vendor"] = json!("Intel")),
        with(&|v| v["vendor"] = json!("8086")),
        with(&|v| v["vendor"] = json!(32902)),
        with(&|v| v["vendor"] = json!(["intel"])),
        with(&|v| {
            v.as_object_mut().unwrap().remove("vendor");
        }),
        // Backends wgpu has but the window doesn't use, or other spellings.
        with(&|v| v["backend"] = json!("gl")),
        with(&|v| v["backend"] = json!("metal")),
        with(&|v| v["backend"] = json!("DX12")),
        with(&|v| v["backend"] = json!("auto")),
        with(&|v| v["backend"] = json!(null)),
        with(&|v| {
            v.as_object_mut().unwrap().remove("backend");
        }),
        // Ranges: empty, null, reversed, not four numbers, unknown keys.
        with(&|v| v["driver"] = json!({})),
        with(&|v| v["driver"] = json!(null)),
        with(&|v| v["driver"] = json!("31.0.101.x")),
        with(&|v| v["driver"]["min"] = json!(null)),
        with(&|v| v["driver"]["max"] = json!("31.0.100.0")),
        with(&|v| v["driver"]["min"] = json!("31.0.101")),
        with(&|v| v["driver"]["max"] = json!("31.0.101.65535.1")),
        with(&|v| v["driver"]["min"] = json!("31.0.101.x")),
        with(&|v| v["driver"]["min"] = json!("")),
        with(&|v| v["driver"]["min"] = json!(31)),
        with(&|v| v["driver"]["max"] = json!("31.0.101.99999999999")),
        with(&|v| v["driver"]["exact"] = json!("31.0.101.2111")),
        // A constraint this app doesn't know would be dropped, widening the
        // rule: the whole rule goes instead.
        with(&|v| v["windows"] = json!({ "min": "26100.0", "max": "26100.9" })),
        with(&|v| v["device"] = json!("9bc4")),
        json!(null),
        json!("intel dx12"),
        json!([]),
    ];
    for bad in bad {
        assert_eq!(renderer_rule(bad.clone()), None, "{bad}");
    }
}

#[test]
fn too_many_renderer_rules_apply_none() {
    let one = json!({ "vendor": "intel", "backend": "dx12" });
    let config = |rules: Value| {
        parse_config(Some(&json!({ "schema": 1, "renderer_rules": rules })))
            .unwrap()
            .renderer_rules
    };
    assert_eq!(config(json!(vec![one.clone(); MAX_RULES])).len(), MAX_RULES);
    assert!(config(json!(vec![one.clone(); MAX_RULES + 1])).is_empty());
    // A bad rule among good ones drops only itself, as second_screen_rules.
    let mixed = config(json!([
        { "vendor": "intel", "backend": "gl" },
        one,
        "garbage",
        { "vendor": "amd", "backend": "vulkan", "driver": { "max": "31.0.21001.45099" } }
    ]));
    assert_eq!(mixed.len(), 2);
    assert_eq!(mixed[0].backend, RendererBackend::Dx12);
    assert_eq!(mixed[1].backend, RendererBackend::Vulkan);
    for not_a_list in [json!(one), json!("intel"), json!(null), json!(8)] {
        assert!(config(not_a_list.clone()).is_empty(), "{not_a_list}");
    }
    // The other values stay when the rules are bad, and the reverse.
    let both = parse_config(Some(&json!({
        "schema": 1,
        "attach_timeout_s": 45,
        "renderer_rules": "garbage",
        "second_screen_rules": [{ "gpus": ["intel"] }]
    })))
    .unwrap();
    assert_eq!(both.attach_timeout_s, Some(45));
    assert!(both.renderer_rules.is_empty() && both.rules.is_empty());
    let rules_only = parse_config(Some(&json!({ "schema": 1, "renderer_rules": [one] }))).unwrap();
    assert_eq!(rules_only.attach_timeout_s, None);
    assert_eq!(rules_only.renderer_rules.len(), 1);
}

#[test]
fn saved_renderer_rules_apply_until_the_config_does_not() {
    let path = temp_file("renderer");
    assert!(read(&path, START).renderer_rules().is_empty());
    write(&path, EXAMPLE.as_bytes(), START).unwrap();
    let applied = read(&path, START + DAY);
    let rules = applied.renderer_rules();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].matches(INTEL, "31.0.101.2111"));
    assert_eq!(rules[0].backend, RendererBackend::Dx12);
    // The launch line is unchanged; the renderer logs its own.
    assert_eq!(
        applied.summary(Some(true)),
        "remote_config=applied rollout=7d ss_rule=match attach_s=45"
    );
    assert!(read(&path, START + MAX_AGE + 1).renderer_rules().is_empty());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
