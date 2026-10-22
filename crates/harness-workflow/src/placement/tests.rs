use std::time::Duration;

use harness_remote::probe::Gpu;

use super::*;

fn probe(cpus: u32, mem_gb: u64, gpus: usize, load: f64) -> ProbeReport {
    ProbeReport {
        helper_version: "1".into(),
        arch: "x86_64".into(),
        os: "linux".into(),
        cpus,
        load: Some([load, load, load]),
        mem_total_bytes: Some(mem_gb * 1024 * 1024 * 1024),
        mem_available_bytes: Some(mem_gb * 1024 * 1024 * 1024 / 2),
        gpus: (0..gpus)
            .map(|i| Gpu {
                name: format!("gpu{i}"),
                memory_total_mib: 1,
                memory_used_mib: 0,
                utilization_pct: None,
            })
            .collect(),
        disk: None,
        jobs: 0,
    }
}

fn pool() -> Arc<HostPool> {
    Arc::new(HostPool::new(vec![
        HostInfo::new("small", vec!["gpu".into()], 1).with_probe(probe(4, 16, 1, 0.1)),
        HostInfo::new("big", vec!["gpu".into(), "bigmem".into()], 2)
            .with_probe(probe(32, 128, 4, 8.0)),
        HostInfo::new("cpu", vec![], 1),
    ]))
}

fn sel(labels: &[&str]) -> HostSelector {
    HostSelector {
        labels: labels.iter().map(|s| (*s).to_owned()).collect(),
        ..HostSelector::default()
    }
}

#[tokio::test]
async fn picks_by_labels_and_resource_minimums() {
    let p = pool();
    let c = CancellationToken::new();
    let mut s = sel(&["gpu"]);
    s.min_mem_gb = Some(64);
    assert_eq!(p.candidates(&s), ["big"]);
    let lease = p.acquire(&RunsOn::Select(s), &c, || {}).await.unwrap();
    assert_eq!(lease.placement(), &Placement::Host("big".into()));
    let mut g = sel(&[]);
    g.min_gpus = Some(2);
    assert_eq!(p.candidates(&g), ["big"]);
    let mut cpus = sel(&[]);
    cpus.min_cpus = Some(8);
    assert_eq!(
        p.candidates(&cpus),
        ["big"],
        "unprobed hosts cannot prove minimums"
    );
    assert_eq!(p.candidates(&sel(&[])).len(), 3);
}

#[tokio::test]
async fn prefers_less_busy_hosts_and_respects_max_jobs() {
    let p = pool();
    let c = CancellationToken::new();
    let any_gpu = RunsOn::Select(sel(&["gpu"]));
    // Both idle: lower load per cpu wins (small: 0.1/4 < big: 8/32).
    let a = p.acquire(&any_gpu, &c, || {}).await.unwrap();
    assert_eq!(a.placement().host(), Some("small"));
    // small is full (max_jobs 1): big takes the next two.
    let b = p.acquire(&any_gpu, &c, || {}).await.unwrap();
    let b2 = p.acquire(&any_gpu, &c, || {}).await.unwrap();
    assert_eq!(
        (b.placement().host(), b2.placement().host()),
        (Some("big"), Some("big"))
    );
    assert_eq!(p.running("big"), 2);
    // Everything is full: the next one waits until a slot frees.
    let p2 = p.clone();
    let c2 = c.clone();
    let waited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = waited.clone();
    let task = tokio::spawn(async move {
        p2.acquire(&RunsOn::Select(sel(&["gpu"])), &c2, move || {
            w.store(true, std::sync::atomic::Ordering::SeqCst)
        })
        .await
        .map(|l| l.placement().clone())
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!task.is_finished());
    assert!(waited.load(std::sync::atomic::Ordering::SeqCst));
    drop(a);
    assert_eq!(
        task.await.unwrap().unwrap(),
        Placement::Host("small".into())
    );
    drop(b);
    assert_eq!(p.running("big"), 1);
}

#[tokio::test]
async fn named_hosts_fallbacks_and_errors() {
    let p = pool();
    let c = CancellationToken::new();
    let l = p
        .acquire(&RunsOn::Host("cpu".into()), &c, || {})
        .await
        .unwrap();
    assert_eq!(l.placement().host(), Some("cpu"));
    drop(l);
    assert_eq!(
        p.acquire(&RunsOn::Host("nope".into()), &c, || {})
            .await
            .unwrap_err(),
        PlacementError::UnknownHost("nope".into())
    );
    let mut tpu = sel(&["tpu"]);
    tpu.fallback = Some(Fallback::Local);
    let l = p
        .acquire(&RunsOn::Select(tpu.clone()), &c, || {})
        .await
        .unwrap();
    assert_eq!(l.placement(), &Placement::Local);
    tpu.fallback = Some(Fallback::Host("cpu".into()));
    let l = p
        .acquire(&RunsOn::Select(tpu.clone()), &c, || {})
        .await
        .unwrap();
    assert_eq!(l.placement().host(), Some("cpu"));
    tpu.fallback = None;
    let e = p
        .acquire(&RunsOn::Select(tpu), &c, || {})
        .await
        .unwrap_err();
    assert!(e.to_string().contains("small: missing labels tpu"), "{e}");
    assert_eq!(
        p.acquire(&RunsOn::Local, &c, || {})
            .await
            .unwrap()
            .placement(),
        &Placement::Local
    );
}

#[tokio::test]
async fn unreachable_hosts_are_skipped() {
    let p = pool();
    p.update_probe("small", None);
    assert_eq!(p.candidates(&sel(&["gpu"])), ["big"]);
    p.update_probe("small", Some(probe(4, 16, 1, 0.0)));
    assert_eq!(p.candidates(&sel(&["gpu"])).len(), 2);
}

#[test]
fn from_registry_reads_ini_hosts_and_probe_cache() {
    let text = "[remote.gpu-box]\nhost = g.example\nlabels = gpu\nmax_jobs = 2\n\n[remote.down]\nhost = d.example\n";
    let cfg = harness_config::ini::load_str(text, None).unwrap();
    let reg = HostRegistry::from_config(&cfg);
    let cache = ProbeCache::new(Duration::from_secs(30));
    cache.put("gpu-box", Ok(probe(8, 64, 1, 0.0)));
    cache.put("down", Err("ssh failed".into()));
    let p = HostPool::from_registry(&reg, &cache);
    assert_eq!(p.candidates(&sel(&["gpu"])), ["gpu-box"]);
    assert!(p.candidates(&sel(&[])).iter().all(|h| h != "down"));
}

#[test]
fn agent_runs_on_converts() {
    use harness_config::agents::RunsOn as A;
    assert_eq!(from_agent(&A::Local), RunsOn::Local);
    assert_eq!(from_agent(&A::Remote("g".into())), RunsOn::Host("g".into()));
    let r = from_agent(&A::Labels {
        labels: vec!["gpu".into()],
        min_mem_gb: Some(32),
        min_cpus: None,
        min_gpus: None,
        fallback: None,
    });
    assert_eq!(
        r,
        RunsOn::Select(HostSelector {
            labels: vec!["gpu".into()],
            min_mem_gb: Some(32),
            ..HostSelector::default()
        })
    );
}

#[test]
fn agent_selectors_map_every_field() {
    use harness_config::agents::RunsOn as A;
    let r = from_agent(&A::Labels {
        labels: vec!["gpu".into()],
        min_mem_gb: Some(16),
        min_cpus: Some(8),
        min_gpus: Some(1),
        fallback: Some("box".into()),
    });
    assert_eq!(
        r,
        RunsOn::Select(HostSelector {
            labels: vec!["gpu".into()],
            min_mem_gb: Some(16),
            min_cpus: Some(8),
            min_gpus: Some(1),
            fallback: Some(crate::schema::Fallback::Host("box".into())),
        })
    );
    let RunsOn::Select(s) = from_agent(&A::Labels {
        labels: vec![],
        min_mem_gb: None,
        min_cpus: None,
        min_gpus: Some(1),
        fallback: Some("local".into()),
    }) else {
        panic!("a selector")
    };
    assert_eq!(s.fallback, Some(crate::schema::Fallback::Local));
}
