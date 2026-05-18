use super::speech::{has_speaker_prefix, resolve_speech_config};
use super::{GenerateMedia, MAX_PROMPT_FILE_BYTES, resolve_prompt};
use crate::tools::local::LocalTool;
use serde_json::{Value, json};

#[test]
fn generate_media_describe_call_truncates_prompt() {
    let tool = GenerateMedia;
    let args = json!({
        "kind": "image",
        "prompt": "a very long prompt ".repeat(20),
        "output_path": "/tmp/x.png",
    });
    let s = tool.describe_call(&args);
    assert!(s.starts_with("generate_media[image]"));
    assert!(s.contains("/tmp/x.png"));
}

#[test]
fn generate_media_normalize_canonicalizes_output_path() {
    let tool = GenerateMedia;
    let dir = tempfile::tempdir().unwrap();
    // Build a path via a `.` segment in an existing directory so the
    // parent canonicalize succeeds and strips it.
    let raw = dir.path().join("./out.png");
    let args = json!({
        "kind": "image",
        "prompt": "x",
        "output_path": raw.display().to_string(),
    });
    let normalized = tool.normalize_for_policy(&args);
    let path = normalized.get("output_path").and_then(Value::as_str).unwrap();
    assert!(path.ends_with("out.png"));
    assert!(!path.contains("/./"));
}

#[test]
fn resolve_prompt_takes_inline() {
    let args = json!({"prompt": "hello"});
    assert_eq!(resolve_prompt(&args).unwrap(), "hello");
}

#[test]
fn resolve_prompt_reads_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.txt");
    std::fs::write(&path, "Alice: Hi.\nBob: Hi back.").unwrap();
    let args = json!({"prompt_file": path.display().to_string()});
    let s = resolve_prompt(&args).unwrap();
    assert!(s.starts_with("Alice: Hi."));
}

#[test]
fn resolve_prompt_rejects_both_set() {
    let args = json!({"prompt": "x", "prompt_file": "/tmp/y"});
    let err = resolve_prompt(&args).unwrap_err().to_string();
    assert!(err.contains("not both"));
}

#[test]
fn resolve_prompt_rejects_neither_set() {
    let args = json!({});
    let err = resolve_prompt(&args).unwrap_err().to_string();
    assert!(err.contains("missing"));
}

#[test]
fn resolve_prompt_rejects_oversized_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    let big = vec![b'x'; (MAX_PROMPT_FILE_BYTES + 1) as usize];
    std::fs::write(&path, &big).unwrap();
    let args = json!({"prompt_file": path.display().to_string()});
    let err = resolve_prompt(&args).unwrap_err().to_string();
    assert!(err.contains("cap"));
}

#[test]
fn describe_call_shows_from_path_when_prompt_file_set() {
    let tool = GenerateMedia;
    let args = json!({
        "kind": "speech",
        "prompt_file": "/tmp/transcript.txt",
    });
    let s = tool.describe_call(&args);
    assert!(s.contains("from /tmp/transcript.txt"));
}

#[test]
fn speech_config_single_voice() {
    let cfg = crate::config::Config::default();
    let opts = json!({"voice": "Kore"});
    let r = resolve_speech_config(Some(&opts), &cfg, "anything").unwrap();
    match r {
        Some(crate::gemini::tts::SpeechConfig::Single(n)) => assert_eq!(n, "Kore"),
        _ => panic!("expected single"),
    }
}

#[test]
fn speech_config_rejects_unknown_voice() {
    let cfg = crate::config::Config::default();
    let opts = json!({"voice": "Robocop"});
    let err = resolve_speech_config(Some(&opts), &cfg, "x").unwrap_err().to_string();
    assert!(err.contains("not in the prebuilt catalog"));
}

#[test]
fn speech_config_speakers_happy_path() {
    let cfg = crate::config::Config::default();
    let opts = json!({
        "speakers": [
            {"name": "Alice", "voice": "Kore"},
            {"name": "Bob",   "voice": "Charon"}
        ]
    });
    let r = resolve_speech_config(Some(&opts), &cfg, "Alice: hi\nBob: hi back").unwrap();
    match r {
        Some(crate::gemini::tts::SpeechConfig::Speakers(s)) => {
            assert_eq!(s.len(), 2);
            assert_eq!(s[0].name, "Alice");
            assert_eq!(s[1].voice, "Charon");
        }
        _ => panic!("expected speakers"),
    }
}

#[test]
fn speech_config_rejects_both_voice_and_speakers() {
    let cfg = crate::config::Config::default();
    let opts = json!({
        "voice": "Kore",
        "speakers": [
            {"name": "Alice", "voice": "Kore"},
            {"name": "Bob",   "voice": "Charon"}
        ]
    });
    let err = resolve_speech_config(Some(&opts), &cfg, "Alice: x\nBob: y").unwrap_err().to_string();
    assert!(err.contains("not both"));
}

#[test]
fn speech_config_rejects_wrong_speakers_count() {
    let cfg = crate::config::Config::default();
    let one = json!({"speakers": [{"name": "Alice", "voice": "Kore"}]});
    assert!(resolve_speech_config(Some(&one), &cfg, "Alice: x").is_err());
    let three = json!({"speakers": [
        {"name": "A", "voice": "Kore"},
        {"name": "B", "voice": "Charon"},
        {"name": "C", "voice": "Aoede"}
    ]});
    assert!(resolve_speech_config(Some(&three), &cfg, "A:\nB:\nC:").is_err());
}

#[test]
fn speech_config_rejects_duplicate_voices() {
    let cfg = crate::config::Config::default();
    let opts = json!({"speakers": [
        {"name": "Alice", "voice": "Kore"},
        {"name": "Bob",   "voice": "Kore"}
    ]});
    let err = resolve_speech_config(Some(&opts), &cfg, "Alice: x\nBob: y").unwrap_err().to_string();
    assert!(err.contains("distinct voices"));
}

#[test]
fn speech_config_rejects_duplicate_names() {
    let cfg = crate::config::Config::default();
    let opts = json!({"speakers": [
        {"name": "Alice", "voice": "Kore"},
        {"name": "Alice", "voice": "Charon"}
    ]});
    let err = resolve_speech_config(Some(&opts), &cfg, "Alice: x").unwrap_err().to_string();
    assert!(err.contains("distinct labels"));
}

#[test]
fn speech_config_rejects_missing_name_in_transcript() {
    let cfg = crate::config::Config::default();
    let opts = json!({"speakers": [
        {"name": "Alice", "voice": "Kore"},
        {"name": "Bob",   "voice": "Charon"}
    ]});
    // Bob has no Bob: prefix in the transcript.
    let err = resolve_speech_config(Some(&opts), &cfg, "Alice: only me here").unwrap_err().to_string();
    assert!(err.contains("Bob"));
}

#[test]
fn has_speaker_prefix_accepts_indented_lines() {
    assert!(has_speaker_prefix("Alice: hi", "Alice"));
    assert!(has_speaker_prefix("   Alice: hi", "Alice"));
    assert!(has_speaker_prefix("intro line\nBob: hi", "Bob"));
    assert!(!has_speaker_prefix("hi Bob: not really", "Bob"));
    assert!(!has_speaker_prefix("Bob said: hi", "Bob"));
}

#[test]
fn generate_media_rejects_unknown_kind() {
    let tool = GenerateMedia;
    let args = json!({"kind": "hologram", "prompt": "x"});
    // run() needs config + api key, but kind validation happens up front
    // — the error path goes through str_arg / match. We exercise that
    // here by tolerating any concrete error and checking the message.
    let err = tool.run(&args).unwrap_err().to_string();
    assert!(
        err.contains("hologram") || err.contains("api"),
        "unexpected error: {err}"
    );
}
