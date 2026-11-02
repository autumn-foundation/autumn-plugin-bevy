//! `BevyConfig`, its newtypes and the `[bevy]` table.

use std::time::Duration;

use autumn_plugin_bevy::{BevyConfig, ConfigError, QueueCapacity, TickMode, TickRate};

// ── AC-22 ──────────────────────────────────────────────────────────────────

#[test]
fn defaults_match_docs() {
    let config = BevyConfig::default();
    assert_eq!(
        config.tick_mode,
        TickMode::Fixed(TickRate::new(60).unwrap())
    );
    assert_eq!(config.queue_capacity.get(), 1024);
    assert_eq!(config.command_timeout, Some(Duration::from_secs(5)));
    assert_eq!(config.startup_timeout, Duration::from_secs(10));
    assert_eq!(config.stall_threshold, Duration::from_secs(1));
    assert_eq!(config.shutdown_timeout, Duration::from_secs(5));
    assert_eq!(config.manual_time_step, Duration::from_nanos(16_666_666));
    assert_eq!(BevyConfig::manual().tick_mode, TickMode::Manual);
    assert_eq!(TickRate::default(), TickRate::DEFAULT);
    assert_eq!(QueueCapacity::default(), QueueCapacity::DEFAULT);
}

#[test]
fn tick_rate_bounds() {
    assert_eq!(TickRate::new(0), Err(ConfigError::TickRate(0)));
    assert_eq!(TickRate::new(1001), Err(ConfigError::TickRate(1001)));
    assert_eq!(TickRate::new(1).unwrap().period(), Duration::from_secs(1));
    assert_eq!(
        TickRate::new(1000).unwrap().period(),
        Duration::from_millis(1)
    );
    let sixty = TickRate::new(60).unwrap();
    assert_eq!(sixty.hz(), 60);
    assert_eq!(sixty.period(), Duration::from_nanos(16_666_666));
}

#[test]
fn queue_capacity_bounds() {
    assert_eq!(QueueCapacity::new(0), Err(ConfigError::QueueCapacity(0)));
    assert_eq!(
        QueueCapacity::new(65_537),
        Err(ConfigError::QueueCapacity(65_537))
    );
    assert_eq!(QueueCapacity::new(1).unwrap().get(), 1);
    assert_eq!(QueueCapacity::new(65_536).unwrap().get(), 65_536);
}

#[test]
fn toml_section_overrides_defaults() {
    let config = BevyConfig::from_toml_str(
        r#"
        [server]
        port = 3000

        [bevy]
        tick_mode = "manual"
        tick_hz = 30
        queue_capacity = 64
        command_timeout_ms = 250
        startup_timeout_ms = 2000
        stall_threshold_ms = 300
        shutdown_timeout_ms = 900
        manual_time_step_ms = 10
        "#,
    )
    .unwrap();
    assert_eq!(config.tick_mode, TickMode::Manual);
    assert_eq!(config.queue_capacity.get(), 64);
    assert_eq!(config.command_timeout, Some(Duration::from_millis(250)));
    assert_eq!(config.startup_timeout, Duration::from_secs(2));
    assert_eq!(config.stall_threshold, Duration::from_millis(300));
    assert_eq!(config.shutdown_timeout, Duration::from_millis(900));
    assert_eq!(config.manual_time_step, Duration::from_millis(10));

    let fixed = BevyConfig::from_toml_str("[bevy]\ntick_hz = 30\n").unwrap();
    assert_eq!(fixed.tick_mode, TickMode::Fixed(TickRate::new(30).unwrap()));
}

#[test]
fn toml_unknown_key_is_named() {
    let err = BevyConfig::from_toml_str("[bevy]\ntick_hertz = 30\n").unwrap_err();
    assert!(matches!(err, ConfigError::Parse(_)), "{err:?}");
    assert!(err.to_string().contains("tick_hertz"), "{err}");
}

#[test]
fn toml_bad_values_are_named() {
    let err = BevyConfig::from_toml_str("[bevy]\ntick_hz = 0\n").unwrap_err();
    assert_eq!(err, ConfigError::TickRate(0));
    let err = BevyConfig::from_toml_str("[bevy]\nqueue_capacity = 0\n").unwrap_err();
    assert_eq!(err, ConfigError::QueueCapacity(0));
    let err = BevyConfig::from_toml_str("[bevy]\ntick_mode = \"turbo\"\n").unwrap_err();
    assert!(err.to_string().contains("turbo"), "{err}");
    let err = BevyConfig::from_toml_str("[bevy]\ntick_hz = \"fast\"\n").unwrap_err();
    assert!(err.to_string().contains("tick_hz"), "{err}");
    let err = BevyConfig::from_toml_str("[bevy\n").unwrap_err();
    assert!(matches!(err, ConfigError::Parse(_)), "{err:?}");
}

#[test]
fn toml_zero_duration_is_named() {
    for key in [
        "command_timeout_ms",
        "startup_timeout_ms",
        "stall_threshold_ms",
        "shutdown_timeout_ms",
        "manual_time_step_ms",
    ] {
        let err = BevyConfig::from_toml_str(&format!("[bevy]\n{key} = 0\n")).unwrap_err();
        assert_eq!(err, ConfigError::ZeroDuration { key }, "{key}");
        assert!(err.to_string().contains(key));
    }
}

#[test]
fn missing_section_uses_defaults() {
    assert_eq!(
        BevyConfig::from_toml_str("").unwrap(),
        BevyConfig::default()
    );
    assert_eq!(
        BevyConfig::from_toml_str("[server]\nport = 1\n").unwrap(),
        BevyConfig::default()
    );
    let err = BevyConfig::from_toml_str("bevy = 3\n").unwrap_err();
    assert!(err.to_string().contains("bevy"), "{err}");
}

#[test]
fn setter_after_load_wins() {
    let config = BevyConfig::from_toml_str("[bevy]\nqueue_capacity = 64\n")
        .unwrap()
        .with_queue_capacity(QueueCapacity::new(8).unwrap())
        .with_command_timeout(None)
        .with_tick_mode(TickMode::Manual)
        .with_startup_timeout(Duration::from_secs(1))
        .with_stall_threshold(Duration::from_secs(2))
        .with_shutdown_timeout(Duration::from_secs(3))
        .with_manual_time_step(Duration::from_millis(4));
    assert_eq!(config.queue_capacity.get(), 8);
    assert_eq!(config.command_timeout, None);
    assert_eq!(config.tick_mode, TickMode::Manual);
    assert_eq!(config.startup_timeout, Duration::from_secs(1));
    assert_eq!(config.stall_threshold, Duration::from_secs(2));
    assert_eq!(config.shutdown_timeout, Duration::from_secs(3));
    assert_eq!(config.manual_time_step, Duration::from_millis(4));
}

#[test]
fn load_reads_a_file_and_a_missing_file_gives_defaults() {
    let dir = std::env::temp_dir().join(format!("autumn-plugin-bevy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("autumn.toml");
    std::fs::write(&path, "[bevy]\ntick_hz = 20\n").unwrap();
    let loaded = BevyConfig::load(&path).unwrap();
    assert_eq!(
        loaded.tick_mode,
        TickMode::Fixed(TickRate::new(20).unwrap())
    );

    let missing = BevyConfig::load(dir.join("absent.toml")).unwrap();
    assert_eq!(missing, BevyConfig::default());

    let err = BevyConfig::load(&dir).unwrap_err();
    assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
