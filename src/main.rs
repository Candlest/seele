use std::{
    fs::create_dir_all,
    process,
    sync::{Arc, Barrier},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use opentelemetry::{
    sdk::{trace, Resource},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use tokio::{runtime, sync::mpsc, task::spawn_blocking};
use tokio_graceful_shutdown::{errors::SubsystemError, Toplevel};
use tracing::{error, info, warn};
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, Layer};

use crate::{conf::SeeleWorkMode, worker::action};

pub mod cgroup;
mod composer;
mod conf;
mod entities;
mod exchange;
mod shared;
mod worker;

fn main() {
    let pid = process::id();

    let runtime = runtime::Builder::new_multi_thread()
        .worker_threads(conf::CONFIG.thread_counts.runtime)
        .max_blocking_threads(conf::CONFIG.thread_counts.worker)
        .thread_keep_alive(Duration::from_secs(u64::MAX))
        .enable_all()
        .build()
        .expect("Error building tokio runtime");
    runtime
        .block_on(async move {
            // tracing::subscriber::set_global_default(
            //     tracing_subscriber::fmt()
            //         .compact()
            //         .with_line_number(true)
            //         .with_max_level(tracing::Level::DEBUG)
            //         .finish(),
            // )
            // .expect("Failed to initialize the logger");

            {
                let tracer = opentelemetry_otlp::new_pipeline()
                    .tracing()
                    .with_exporter(
                        opentelemetry_otlp::new_exporter()
                            .tonic()
                            .with_endpoint("http://192.168.20.218:4317")
                            .with_timeout(Duration::from_secs(5)), // with metadata x-hostname
                    )
                    .with_trace_config(trace::config().with_resource(Resource::new(vec![
                        KeyValue::new("service.name", "seele"),
                        KeyValue::new("service.host", "local"),
                    ])))
                    .install_batch(opentelemetry::runtime::Tokio)
                    .expect("Error initializing the tracer");

                tracing::subscriber::set_global_default(
                    tracing_subscriber::registry()
                        .with(
                            tracing_opentelemetry::layer()
                                .with_tracer(tracer)
                                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
                        )
                        .with(
                            tracing_subscriber::fmt::layer()
                                .compact()
                                .with_line_number(true)
                                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
                        ),
                )
                .expect("Error initializing the tracing subscriber");
            }

            spawn_blocking(|| -> Result<()> {
                info!("Checking cpu counts");
                let logical_cpu_count = num_cpus::get();
                let physical_cpu_count = num_cpus::get_physical();
                if physical_cpu_count < logical_cpu_count {
                    // TODO: Add link to document
                    warn!(
                        "Seele does not recommand enabling the cpu's SMT technology, current \
                         logical cpu count: {}, physical cpu count: {}",
                        logical_cpu_count, physical_cpu_count
                    )
                }

                if !matches!(&conf::CONFIG.work_mode, SeeleWorkMode::RootlessContainerized) {
                    info!("Checking id maps");
                    if action::run_container::SUBUIDS.count < 65536 {
                        bail!(
                            "The user specified in the run_container namespace config has not \
                             enough subuid mapping range"
                        );
                    }
                    if action::run_container::SUBGIDS.count < 65536 {
                        bail!(
                            "The group specified in the run_container namespace config has not \
                             enough subgid mapping range"
                        );
                    }
                }

                info!("Checking cgroup setup");
                cgroup::check_cgroup_setup().context("Error checking cgroup setup")?;

                info!("Initializing cgroup subtrees");
                cgroup::initialize_cgroup_subtrees()
                    .context("Error initializing cgroup subtrees")?;

                info!("Creating necessary directories in {}", conf::PATHS.root.display());
                for path in [
                    &conf::PATHS.images,
                    &conf::PATHS.submissions,
                    &conf::PATHS.evicted,
                    &conf::PATHS.temp,
                ] {
                    create_dir_all(path).with_context(|| {
                        format!("Error creating the directory: {}", path.display())
                    })?;
                }

                Ok(())
            })
            .await??;

            {
                info!("Binding application threads to cpus");
                let worker_count = conf::CONFIG.thread_counts.worker;
                let begin_barrier = Arc::new(Barrier::new(worker_count));
                let end_barrier = Arc::new(Barrier::new(worker_count));

                for _ in 0..(worker_count - 1) {
                    let begin_barrier = begin_barrier.clone();
                    let end_barrier = end_barrier.clone();
                    spawn_blocking(move || {
                        begin_barrier.wait();
                        end_barrier.wait();
                    });
                }

                spawn_blocking(move || {
                    begin_barrier.wait();
                    let result = cgroup::bind_application_threads(pid);
                    end_barrier.wait();
                    result
                })
                .await
                .unwrap()
                .expect("Error binding application threads");
            }

            let (composer_queue_tx, composer_queue_rx) =
                mpsc::channel::<composer::ComposerQueueItem>(16);
            let (worker_queue_tx, worker_queue_rx) =
                async_channel::bounded::<worker::WorkerQueueItem>(16);

            let result = Toplevel::new()
                .start("exchange", |handle| exchange::exchange_main(handle, composer_queue_tx))
                .start("composer", |handle| {
                    composer::composer_main(handle, composer_queue_rx, worker_queue_tx)
                })
                .start("worker", |handle| worker::worker_main(handle, worker_queue_rx))
                .catch_signals()
                .handle_shutdown_requests(Duration::from_secs(10))
                .await;
            if let Err(err) = result {
                error!("Seele encountered fatal issue(s):");
                for error in err.get_subsystem_errors() {
                    match error {
                        SubsystemError::Failed(name, err) => {
                            error!("Subsystem '{}' failed: {:?}", name, err);
                        }
                        SubsystemError::Cancelled(name) => {
                            error!("Subsystem '{}' was cancelled", name);
                        }
                        SubsystemError::Panicked(name) => {
                            error!("Subsystem '{}' panicked", name);
                        }
                    }
                }
            }

            anyhow::Ok(())
        })
        .expect("Error initializing runtime");
}
