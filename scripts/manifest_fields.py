"""The optional docs/latest.json fields both apps read (docs/remote-config.md).

check-versions.sh and latest_json.py both call check(), so a manifest one
accepts the other does too. The limits match host/src/remote_config.rs,
host/src/updates.rs and ios/EternalMonitor/Core/RemoteConfig.swift and
UpdateChecker.swift; the apps use their defaults for anything else.
"""
import datetime
import re
import unicodedata


def integer(value):
    return type(value) is int


def check_note(where, note):
    if note is None:
        return
    # Control, format (U+200B, U+202E…) and line or paragraph separators:
    # both apps drop a note with one, so it must not be published.
    if (not isinstance(note, str) or not note.strip() or len(note.strip()) > 200
            or any(unicodedata.category(c) in ("Cc", "Cf", "Zl", "Zp") for c in note)):
        raise ValueError(f"{where}.note must be one line of at most 200 characters")


def check_rollout(where, rollout):
    if rollout is None:
        return
    if not isinstance(rollout, dict) or set(rollout) != {"start", "days"}:
        raise ValueError(f"{where}.rollout must be {{\"start\": ..., \"days\": ...}}")
    start = rollout["start"]
    try:
        if not isinstance(start, str) or not re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", start):
            raise ValueError
        when = datetime.datetime.strptime(start, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=datetime.timezone.utc)
    except ValueError:
        raise ValueError(f"{where}.rollout.start must be a UTC time like 2026-11-02T00:00:00Z") from None
    # The apps show the prompt at once for a start this far ahead, as a typo.
    if when > datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=14):
        raise ValueError(f"{where}.rollout.start must be at most 14 days from now")
    if not integer(rollout["days"]) or not 0 <= rollout["days"] <= 14:
        raise ValueError(f"{where}.rollout.days must be a whole number from 0 to 14")


def check_config(where, config, values):
    if config is None:
        return
    if not isinstance(config, dict) or config.get("schema") != 1 or not integer(config["schema"]):
        raise ValueError(f"{where}.config must be an object with \"schema\": 1")
    unknown = set(config) - {"schema", *values}
    if unknown:
        raise ValueError(f"{where}.config has unknown keys {sorted(unknown)}")
    for key, bounds in values.items():
        if bounds and key in config and (not integer(config[key])
                                         or not bounds[0] <= config[key] <= bounds[1]):
            raise ValueError(f"{where}.config.{key} must be a whole number from {bounds[0]} to {bounds[1]}")


def version_numbers(text):
    if not isinstance(text, str) or not re.fullmatch(r"\d+(\.\d+){0,3}", text):
        return None
    parts = [int(p) for p in text.split(".")]
    return None if any(p > 0xFFFFFFFF for p in parts) else parts

# The same limits as host/src/remote_config.rs (docs/remote-config.md):
# `parts` numbers at each end, equal but for the last, which rises by at
# most `span`.
def check_range(where, value, keys, parts, span, shape):
    if not isinstance(value, dict) or set(value) != keys:
        raise ValueError(f"{where} must have exactly {sorted(keys)}")
    low, high = version_numbers(value["min"]), version_numbers(value["max"])
    if (not low or not high or len(low) != len(high) or len(low) not in parts
            or low[:-1] != high[:-1] or not 0 <= high[-1] - low[-1] <= span):
        raise ValueError(f"{where} must be {shape}")


def check_rules(rules):
    if rules is None:
        return
    if not isinstance(rules, list) or len(rules) > 8:
        raise ValueError("windows.config.second_screen_rules must be a list of at most 8 rules")
    vendors = {"amd", "intel", "nvidia", "qualcomm"}
    for index, rule in enumerate(rules):
        where = f"windows.config.second_screen_rules[{index}]"
        if not isinstance(rule, dict):
            raise ValueError(f"{where} must be an object")
        unknown = set(rule) - {"gpus", "gpu_driver", "display_driver", "windows"}
        if unknown:
            raise ValueError(f"{where} has unknown keys {sorted(unknown)}")
        gpus = rule.get("gpus")
        if (not isinstance(gpus, list) or not gpus or len(set(map(str, gpus))) != len(gpus)
                or not all(isinstance(g, str) and g in vendors for g in gpus)):
            raise ValueError(f"{where}.gpus must list distinct vendors from {sorted(vendors)}")
        if "gpu_driver" not in rule:
            raise ValueError(f"{where} must name a graphics driver release (gpu_driver)")
        if not {"display_driver", "windows"} & set(rule):
            raise ValueError(f"{where} must also name a display_driver or windows range")
        check_range(f"{where}.gpu_driver", rule["gpu_driver"], {"gpu", "min", "max"}, {4}, 99,
                    "one driver release: four numbers, equal but for the last, at most 99 apart")
        if rule["gpu_driver"]["gpu"] not in gpus:
            raise ValueError(f"{where}.gpu_driver.gpu must be one of its gpus")
        if "display_driver" in rule:
            check_range(f"{where}.display_driver", rule["display_driver"], {"min", "max"},
                        {2, 3, 4}, float("inf"), "an exact version or patch range, like 25.7.0 to 25.7.99")
        if "windows" in rule:
            check_range(f"{where}.windows", rule["windows"], {"min", "max"}, {2}, float("inf"),
                        "one Windows build with an update range, like 26100.0 to 26100.2999")


def check(windows, ipad):
    """Raise ValueError naming the first bad optional field."""
    known = {
        "windows": {"version", "installer", "sha256", "recommended", "note", "rollout", "config"},
        "ipad": {"version", "build", "testflight", "recommended", "note", "rollout", "config"},
    }
    for where, entry in (("windows", windows), ("ipad", ipad)):
        unknown = set(entry) - known[where]
        if unknown:
            raise ValueError(f"{where} has unknown keys {sorted(unknown)}")
        if "recommended" in entry and not isinstance(entry["recommended"], bool):
            raise ValueError(f"{where}.recommended must be true or false")
        check_note(where, entry.get("note"))
        check_rollout(where, entry.get("rollout"))
    check_config("windows", windows.get("config"),
                 {"attach_timeout_s": (30, 60), "second_screen_rules": None})
    check_rules((windows.get("config") or {}).get("second_screen_rules"))
    check_config("ipad", ipad.get("config"), {"first_frame_timeout_s": (10, 20)})
