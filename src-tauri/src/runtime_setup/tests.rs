use super::download::tests::{serve, sha_hex};
use super::manifest::{ModelFile, ModelPins};
use super::*;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wren-setup-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn layout_in(root: &Path) -> Layout {
    let paths = Paths::under(root);
    Layout {
        runtime: paths.local.join("runtime"),
        models: paths.local.join("models"),
        cache: paths.local.join("setup-cache"),
        log: paths.local.join("logs").join("runtime-setup.log"),
        sidecar_dir: crate::engine::resolve_sidecar_dir(),
        hf_base: "http://127.0.0.1:9".into(),
    }
}

fn touch(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"x").unwrap();
}

/// Puts a fake interpreter and every pinned main/fallback model's required file in place.
fn fake_installed(l: &Layout) {
    touch(&venv_python(&l.venv()));
    if let Some((pins, _)) = manifest::platform() {
        for name in &pins.models {
            let m = &manifest::manifest().models[name];
            touch(&l.models.join(name).join(&m.required[0]));
        }
    }
}

#[test]
fn missing_files_are_named_and_custom_models_are_left_alone() {
    let root = temp("missing");
    let l = layout_in(&root);
    let s = EngineSettings::default();
    let python = venv_python(&l.venv());
    assert!(engine_files_missing(&python, &l.models, &s).unwrap().contains("isn't set up"));
    touch(&python);
    if manifest::platform().is_some() {
        assert!(engine_files_missing(&python, &l.models, &s).unwrap().contains("model"));
    }
    fake_installed(&l);
    assert_eq!(engine_files_missing(&python, &l.models, &s), None);
    // A model name Wren doesn't pin is the user's own: never reported as "needs setup".
    let custom = EngineSettings { model: "my-own-model".into(), ..EngineSettings::default() };
    let _ = std::fs::remove_dir_all(&l.models);
    assert_eq!(engine_files_missing(&python, &l.models, &custom), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plan_counts_only_what_is_missing() {
    if manifest::platform().is_none() {
        return;
    }
    let root = temp("plan");
    let l = layout_in(&root);
    let p = plan(&l);
    assert!(p.supported);
    assert_eq!(p.parts.len(), 3, "runtime + main model + fallback");
    assert!(p.parts.iter().all(|x| !x.done));
    assert_eq!(p.download_bytes, p.parts.iter().map(|x| x.bytes).sum::<u64>());
    assert!(p.download_bytes > 900_000_000, "a realistic size: {}", p.download_bytes);
    assert!(p.needed_bytes > p.download_bytes, "space includes the unpacked runtime");
    assert!(p.free_bytes.is_some_and(|f| f > 0));
    assert!(p.location.ends_with("local"), "{}", p.location);

    fake_installed(&l);
    let p = plan(&l);
    assert!(p.parts.iter().all(|x| x.done));
    assert_eq!((p.download_bytes, p.needed_bytes), (0, 0));

    // Only the fallback missing: only its bytes are counted.
    let (pins, _) = manifest::platform().unwrap();
    std::fs::remove_dir_all(l.models.join(&pins.models[1])).unwrap();
    let p = plan(&l);
    assert_eq!(p.download_bytes, manifest::manifest().models[&pins.models[1]].bytes());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn swap_puts_the_new_venv_live_and_never_loses_the_old_on_failure() {
    let root = temp("swap");
    let l = layout_in(&root);
    touch(&venv_python(&l.venv()));
    std::fs::write(l.venv().join("which"), "old").unwrap();

    // An incomplete staging venv is refused and the live one is untouched.
    std::fs::create_dir_all(l.staging()).unwrap();
    assert!(swap_venv(&l).is_err());
    assert_eq!(std::fs::read_to_string(l.venv().join("which")).unwrap(), "old");

    touch(&venv_python(&l.staging()));
    std::fs::write(l.staging().join("which"), "new").unwrap();
    swap_venv(&l).unwrap();
    assert_eq!(std::fs::read_to_string(l.venv().join("which")).unwrap(), "new");
    assert!(!l.staging().exists());
    let leftovers: Vec<_> = std::fs::read_dir(&l.runtime).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from(".venv")], "old venv removed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_interrupted_swap_restores_the_old_runtime() {
    let root = temp("interrupted");
    let l = layout_in(&root);
    // Wren stopped after renaming .venv away and before the new one went live.
    let old = l.runtime.join(".venv.old-123");
    touch(&venv_python(&old));
    cleanup_stale(&l);
    assert!(venv_python(&l.venv()).is_file(), "restored");
    assert!(!old.exists());
    // With a live venv, old copies are just removed.
    touch(&venv_python(&l.runtime.join(".venv.old-456")));
    cleanup_stale(&l);
    assert!(!l.runtime.join(".venv.old-456").exists() && venv_python(&l.venv()).is_file());
    let _ = std::fs::remove_dir_all(&root);
}

fn pins(files: &[(&str, &[u8])]) -> ModelPins {
    ModelPins {
        repo: "org/model".into(),
        revision: "abc123".into(),
        required: vec![files[0].0.to_string()],
        files: files.iter().map(|(n, b)| ModelFile { name: n.to_string(), size: b.len() as u64, sha256: sha_hex(b) }).collect(),
    }
}

#[test]
fn a_model_downloads_resumes_verifies_and_lands_with_its_marker() {
    let big: Vec<u8> = (0..600_000u32).map(|i| (i % 253) as u8).collect();
    let small = b"{\"a\":1}".to_vec();
    let m = pins(&[("model.bin", &big), ("config.json", &small)]);
    // The first request is cut off part-way, like a dropped connection.
    let srv = serve(
        vec![("/org/model/resolve/abc123/model.bin".into(), big.clone()), ("/org/model/resolve/abc123/config.json".into(), small.clone())],
        Some(200_000),
        true,
    );
    let root = temp("model");
    let l = layout_in(&root);
    let client = download::client().unwrap();
    let never = AtomicBool::new(false);
    let quiesced = AtomicBool::new(false);
    let err = install_model(&client, &srv.url, "tiny", &m, &l.models, &l.cache, &never, &mut |_| {}, &|| {
        quiesced.store(true, Ordering::SeqCst)
    })
    .unwrap_err();
    assert!(format!("{err:#}").contains("try again"), "{err:#}");
    assert!(!l.models.join("tiny").exists(), "nothing half-installed");
    assert!(!quiesced.load(Ordering::SeqCst), "the engine is not stopped for a failed download");

    let mut seen = Vec::new();
    install_model(&client, &srv.url, "tiny", &m, &l.models, &l.cache, &never, &mut |b| seen.push(b), &|| {
        quiesced.store(true, Ordering::SeqCst)
    })
    .unwrap();
    let dir = l.models.join("tiny");
    assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), big);
    assert_eq!(std::fs::read_to_string(dir.join(REVISION_MARKER)).unwrap().trim(), "abc123");
    assert!(m.present_in(&dir));
    assert!(quiesced.load(Ordering::SeqCst));
    assert!(*seen.first().unwrap() > 0, "progress resumed from the kept part, not zero");
    assert_eq!(*seen.last().unwrap(), m.bytes());
    assert!(!l.cache.join("models").join("tiny").exists(), "the staged folder became the model");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_model_folder_wren_did_not_make_is_moved_aside_not_deleted() {
    let data = b"weights".to_vec();
    let m = pins(&[("model.bin", &data)]);
    let srv = serve(vec![("/org/model/resolve/abc123/model.bin".into(), data.clone())], None, true);
    let root = temp("aside");
    let l = layout_in(&root);
    let mine = l.models.join("tiny");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("my-notes.txt"), "keep me").unwrap();
    install_model(&download::client().unwrap(), &srv.url, "tiny", &m, &l.models, &l.cache, &AtomicBool::new(false), &mut |_| {}, &|| {}).unwrap();
    let backup = std::fs::read_dir(&l.models)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("tiny.user-backup-"))
        .expect("moved aside");
    assert_eq!(std::fs::read_to_string(backup.path().join("my-notes.txt")).unwrap(), "keep me");
    assert!(m.present_in(&mine));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_wren_model_with_good_files_reuses_them() {
    let a = b"aaaa".to_vec();
    let b = b"bbbb".to_vec();
    let m = pins(&[("model.bin", &a), ("vocab.txt", &b)]);
    let srv = serve(vec![("/org/model/resolve/abc123/vocab.txt".into(), b.clone())], None, true);
    let root = temp("reuse");
    let l = layout_in(&root);
    let dir = l.models.join("tiny");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.bin"), &a).unwrap();
    std::fs::write(dir.join("vocab.txt"), b"corrupt").unwrap();
    std::fs::write(dir.join(REVISION_MARKER), "abc123").unwrap();
    install_model(&download::client().unwrap(), &srv.url, "tiny", &m, &l.models, &l.cache, &AtomicBool::new(false), &mut |_| {}, &|| {}).unwrap();
    assert_eq!(std::fs::read(dir.join("vocab.txt")).unwrap(), b);
    assert_eq!(srv.requests.load(Ordering::SeqCst), 1, "only the bad file was downloaded");
    let _ = std::fs::remove_dir_all(&root);
}

fn service(l: &Layout) -> (Service, Arc<Mutex<Vec<SetupStatus>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let svc = Service::new(l.clone(), &venv_python(&l.venv()), &EngineSettings::default(), Box::new(move |s| s2.lock().push(s.clone())));
    (svc, seen)
}

#[test]
fn done_only_after_the_real_engine_is_ready() {
    let root = temp("svc");
    let l = layout_in(&root);
    let (svc, seen) = service(&l);
    assert!(matches!(svc.status(), SetupStatus::Needed { .. }));
    // Files in place, engine warming: not done yet.
    svc.inner.set(SetupStatus::Starting { plan: plan(&l) });
    svc.on_engine_status(&EngineStatus::Starting);
    svc.on_engine_status(&EngineStatus::WarmingUp);
    assert!(matches!(svc.status(), SetupStatus::Starting { .. }));
    svc.on_engine_status(&EngineStatus::Ready { model: "large-v3-turbo".into(), device: "cuda".into(), vram_mb: 1 });
    assert_eq!(svc.status(), SetupStatus::Done { model: "large-v3-turbo".into(), device: "cuda".into() });
    assert!(matches!(seen.lock().last(), Some(SetupStatus::Done { .. })), "the UI was told");

    // A failed warm-up after setup is shown as a setup error with the engine's reason.
    svc.inner.set(SetupStatus::Starting { plan: plan(&l) });
    svc.on_engine_status(&EngineStatus::Failed { reason: "no speech model could be loaded".into() });
    match svc.status() {
        SetupStatus::Failed { step, message, paused, .. } => {
            assert_eq!(step, "engine");
            assert!(message.contains("no speech model") && !paused);
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_ready_engine_clears_needed_and_a_lost_runtime_brings_it_back() {
    let root = temp("svc2");
    let l = layout_in(&root);
    let (svc, _) = service(&l);
    svc.on_engine_status(&EngineStatus::Ready { model: "m".into(), device: "cpu".into(), vram_mb: 0 });
    assert_eq!(svc.status(), SetupStatus::NotNeeded);
    svc.on_engine_status(&EngineStatus::NeedsSetup { reason: "gone".into() });
    assert!(matches!(svc.status(), SetupStatus::Needed { reason, .. } if reason == "gone"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_installed_runtime_needs_no_setup() {
    let root = temp("svc3");
    let l = layout_in(&root);
    fake_installed(&l);
    let (svc, _) = service(&l);
    assert_eq!(svc.status(), SetupStatus::NotNeeded);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn status_serialises_for_the_ui() {
    let s = SetupStatus::Failed { plan: plan(&layout_in(Path::new("/tmp/x"))), step: "models".into(), message: "m".into(), paused: true };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["state"], "failed");
    assert_eq!(v["paused"], true);
    assert!(v["plan"]["downloadBytes"].is_u64() && v["plan"]["logPath"].is_string());
    assert_eq!(serde_json::to_value(SetupStatus::NotNeeded).unwrap()["state"], "notNeeded");
}

#[test]
fn user_messages_name_the_step() {
    assert!(user_message("models", &anyhow!("no internet")).starts_with("Couldn't download the speech models: no internet"));
    assert!(user_message("packages", &anyhow!("x")).starts_with("Couldn't install the speech engine"));
    assert_eq!(user_message("space", &anyhow!("Wren needs about 5 GB")), "Wren needs about 5 GB");
}
