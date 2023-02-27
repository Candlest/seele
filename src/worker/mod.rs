use std::{error::Error, fmt::Display, path::PathBuf, sync::Arc};

use anyhow::Result;
use chrono::Utc;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use tokio_graceful_shutdown::SubsystemHandle;
use tracing::{error, info_span, Instrument, Span};
use triggered::Listener;

pub use self::action::*;
use crate::entities::{
    ActionFailedReport, ActionFailedReportExt, ActionReport, ActionSuccessReport, ActionTaskConfig,
};

pub mod action;

#[derive(Debug)]
pub struct WorkerQueueItem {
    pub parent_span: Span,
    pub submission_id: String,
    pub submission_root: PathBuf,
    pub config: Arc<ActionTaskConfig>,
    pub report_tx: oneshot::Sender<ActionReport>,
}

#[derive(Debug, Clone)]
pub struct ActionErrorWithReport {
    report: ActionFailedReportExt,
}

impl ActionErrorWithReport {
    pub fn new(report: ActionFailedReportExt) -> Self {
        Self { report }
    }
}

impl Error for ActionErrorWithReport {}

impl Display for ActionErrorWithReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Action execution failed with a report")
    }
}

pub type WorkerQueueTx = mpsc::Sender<WorkerQueueItem>;
pub type WorkerQueueRx = mpsc::Receiver<WorkerQueueItem>;

pub async fn worker_main(handle: SubsystemHandle, mut queue_rx: WorkerQueueRx) -> Result<()> {
    let (trigger, abort_handle) = triggered::trigger();

    tokio::spawn(async move {
        handle.on_shutdown_requested().await;
        trigger.trigger();
    });

    loop {
        let outer_handle = abort_handle.clone();
        tokio::select! {
            _ = outer_handle => break,
            item = queue_rx.recv() => match item {
                None => break,
                Some(item) => {
                    tokio::spawn({
                        let abort_handle = abort_handle.clone();
                        let span = info_span!(parent: item.parent_span, "worker_handle_submission");
                        async move {
                            let report = execute_action(abort_handle.clone(), item.submission_root, &item.config).await;

                            if item.report_tx.send(report).is_err() {
                                error!(submission_id = item.submission_id, "Error sending the report");
                            }
                        }
                        .instrument(span)
                    });
                }
            }
        }
    }

    Ok(())
}

async fn execute_action(
    handle: Listener,
    submission_root: PathBuf,
    task: &ActionTaskConfig,
) -> ActionReport {
    let ctx = Arc::new(ActionContext { submission_root });

    let begin = Instant::now();
    let run_at = Utc::now();
    let result = match task {
        ActionTaskConfig::Noop(config) => action::noop::execute(config).await,
        ActionTaskConfig::AddFile(config) => action::add_file::execute(handle, &ctx, config).await,
        ActionTaskConfig::RunContainer(config) => {
            action::run_container::execute(handle, &ctx, config).await
        }
        ActionTaskConfig::RunJudgeCompile(config) => {
            action::run_container::run_judge::compile::execute(handle, &ctx, config).await
        }
        ActionTaskConfig::RunJudgeRun(config) => {
            action::run_container::run_judge::run::execute(handle, &ctx, config).await
        }
    };
    let time_elapsed_ms = {
        let end = Instant::now();
        end.duration_since(begin).as_millis().try_into().unwrap()
    };

    match result {
        Err(err) => ActionFailedReport {
            run_at: Some(run_at),
            time_elapsed_ms: Some(time_elapsed_ms),
            error: format!("Error executing the action: {err:#}"),
            ext: err
                .root_cause()
                .downcast_ref::<ActionErrorWithReport>()
                .map(|err| err.report.clone()),
        }
        .into(),
        Ok(ext) => ActionSuccessReport { run_at, time_elapsed_ms, ext }.into(),
    }
}
