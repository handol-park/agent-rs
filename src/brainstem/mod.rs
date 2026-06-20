//! Brainstem: the body + runtime for the actor agent (spec 002 goals 6-13).

use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::spawn_blocking;
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;

use crate::action::RecoverableError;
use crate::budget::BudgetSummary;
use crate::event::{RunEvent, Termination};
use crate::mind::{Command, Decision, Mind, Perception, Reason, TaskFault};
use crate::observation::{Observation, Outcome, TaskOutcome};
use crate::tool::ToolRegistry;

/// A task submitted to the brainstem's inbox.
pub struct Task {
    pub goal: String,
    pub reply: Option<oneshot::Sender<TaskOutcome>>,
}

/// The brainstem's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Idle,
    Working,
    Throttling,
    Cancelled,
    Fatal,
    Stopped,
}

/// A snapshot of the brainstem's state (for Status queries).
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub lifecycle: Lifecycle,
    pub current_task: Option<String>,
    pub tokens_remaining: u64,
    pub next_reset: Instant,
    pub queue_depth: usize,
    pub steps_used: usize,
}

/// The brainstem: drives the perpetual actor loop.
pub struct Brainstem {
    mind: Box<dyn Mind>,
    registry: Arc<ToolRegistry>,
    max_steps: usize,
    inbox: mpsc::Receiver<Task>,
    status_rx: mpsc::Receiver<oneshot::Sender<Snapshot>>,
    cancel: CancellationToken,
    event_tx: mpsc::UnboundedSender<RunEvent>,
}

/// Internal state for building snapshots.
struct BrainstemState {
    lifecycle: Lifecycle,
    current_task: Option<String>,
    budget_summary: BudgetSummary,
    steps_used: usize,
}

impl BrainstemState {
    fn build_snapshot(&self) -> Snapshot {
        Snapshot {
            lifecycle: self.lifecycle,
            current_task: self.current_task.clone(),
            tokens_remaining: self.budget_summary.tokens_remaining,
            next_reset: self.budget_summary.next_reset,
            queue_depth: 0,
            steps_used: self.steps_used,
        }
    }
}

impl Brainstem {
    pub fn new(
        mind: Box<dyn Mind>,
        registry: Arc<ToolRegistry>,
        max_steps: usize,
        inbox: mpsc::Receiver<Task>,
        status_rx: mpsc::Receiver<oneshot::Sender<Snapshot>>,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<RunEvent>,
    ) -> Self {
        Self {
            mind,
            registry,
            max_steps,
            inbox,
            status_rx,
            cancel,
            event_tx,
        }
    }

    /// Run the perpetual drive loop until cancellation, fatal error, or inbox close.
    pub async fn run(mut self) -> Termination {
        let mut state = BrainstemState {
            lifecycle: Lifecycle::Idle,
            current_task: None,
            budget_summary: self.mind.budget_summary(),
            steps_used: 0,
        };

        loop {
            // Idle select: wait for task, status, or cancel
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    state.lifecycle = Lifecycle::Cancelled;
                    let _ = self.event_tx.send(RunEvent::Terminated {
                        reason: Termination::Cancelled,
                    });
                    return Termination::Cancelled;
                }
                Some(reply_tx) = self.status_rx.recv() => {
                    let snapshot = state.build_snapshot();
                    let _ = reply_tx.send(snapshot);
                }
                task = self.inbox.recv() => {
                    match task {
                        None => {
                            // Inbox closed
                            state.lifecycle = Lifecycle::Stopped;
                            let _ = self.event_tx.send(RunEvent::Terminated {
                                reason: Termination::Stopped,
                            });
                            return Termination::Stopped;
                        }
                        Some(task) => {
                            let _ = self.event_tx.send(RunEvent::TaskReceived {
                                goal: task.goal.clone(),
                            });

                            match self.run_episode(task.goal.clone(), &mut state).await {
                                Ok(outcome) => {
                                    let _ = self.event_tx.send(RunEvent::TaskCompleted {
                                        outcome: outcome.clone(),
                                    });
                                    if let Some(tx) = task.reply {
                                        let _ = tx.send(TaskOutcome::Completed(outcome));
                                    }
                                }
                                Err(EpisodeFailure::TaskFatal(reason)) => {
                                    let _ = self.event_tx.send(RunEvent::TaskFailed {
                                        reason: reason.clone(),
                                    });
                                    if let Some(tx) = task.reply {
                                        let _ = tx.send(TaskOutcome::Failed(reason));
                                    }
                                }
                                Err(EpisodeFailure::ServiceFatal(msg)) => {
                                    state.lifecycle = Lifecycle::Fatal;
                                    let _ = self.event_tx.send(RunEvent::Terminated {
                                        reason: Termination::Fatal(msg.clone()),
                                    });
                                    return Termination::Fatal(msg);
                                }
                                Err(EpisodeFailure::Cancelled) => {
                                    state.lifecycle = Lifecycle::Cancelled;
                                    let _ = self.event_tx.send(RunEvent::Terminated {
                                        reason: Termination::Cancelled,
                                    });
                                    return Termination::Cancelled;
                                }
                            }

                            // Reset for next task
                            state.current_task = None;
                            state.steps_used = 0;
                            state.lifecycle = Lifecycle::Idle;
                        }
                    }
                }
            }
        }
    }

    /// Run a single task episode.
    async fn run_episode(
        &mut self,
        goal: String,
        state: &mut BrainstemState,
    ) -> Result<Outcome, EpisodeFailure> {
        state.lifecycle = Lifecycle::Working;
        state.current_task = Some(goal.clone());
        state.steps_used = 0;

        let mut perception = Perception::NewTask { goal };

        loop {
            // Refresh snapshot before decide
            state.budget_summary = self.mind.budget_summary();

            // Decide with cancel/status select
            let decision = {
                let decide_fut = self.mind.decide(perception.clone());
                tokio::pin!(decide_fut);

                loop {
                    tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => {
                            return Err(EpisodeFailure::Cancelled);
                        }
                        Some(reply_tx) = self.status_rx.recv() => {
                            let snapshot = state.build_snapshot();
                            let _ = reply_tx.send(snapshot);
                            // Continue waiting for decide
                        }
                        decision = &mut decide_fut => {
                            break decision;
                        }
                    }
                }
            };

            match decision {
                Decision::Act(cmd) => {
                    state.steps_used += 1;
                    if state.steps_used > self.max_steps {
                        return Err(EpisodeFailure::TaskFatal(TaskFault::NoProgress));
                    }

                    let _ = self.event_tx.send(RunEvent::Command {
                        call_id: match &cmd {
                            Command::CallTool { call_id, .. } => call_id.clone(),
                        },
                        name: match &cmd {
                            Command::CallTool { name, .. } => name.clone(),
                        },
                    });

                    // Actuate command off-loop with spawn_blocking
                    let obs = self.actuate_command(cmd).await?;

                    let ok = matches!(obs, Observation::ToolResult { .. });
                    let call_id = match &obs {
                        Observation::ToolResult { call_id, .. } => call_id.clone(),
                        Observation::Recoverable { call_id, .. } => {
                            call_id.clone().unwrap_or_default()
                        }
                    };

                    let _ = self.event_tx.send(RunEvent::CommandResult { call_id, ok });

                    if let Observation::Recoverable { error, .. } = &obs {
                        let _ = self.event_tx.send(RunEvent::RecoverableObservation {
                            error: error.clone(),
                        });
                    }

                    perception = Perception::Observation(obs);
                }
                Decision::Done(outcome) => {
                    return Ok(outcome);
                }
                Decision::Failed(Reason::Task(fault)) => {
                    return Err(EpisodeFailure::TaskFatal(fault));
                }
                Decision::Failed(Reason::Service(err)) => {
                    return Err(EpisodeFailure::ServiceFatal(err.to_string()));
                }
                Decision::Throttle(wake) => {
                    state.lifecycle = Lifecycle::Throttling;
                    let _ = self.event_tx.send(RunEvent::ThrottleSleep { wake });

                    // Sleep with cancel/status select
                    let sleep_fut = sleep_until(wake);
                    tokio::pin!(sleep_fut);

                    loop {
                        tokio::select! {
                            biased;
                            _ = self.cancel.cancelled() => {
                                return Err(EpisodeFailure::Cancelled);
                            }
                            Some(reply_tx) = self.status_rx.recv() => {
                                let snapshot = state.build_snapshot();
                                let _ = reply_tx.send(snapshot);
                            }
                            _ = &mut sleep_fut => {
                                break;
                            }
                        }
                    }

                    state.lifecycle = Lifecycle::Working;
                    perception = Perception::Resume;
                }
            }
        }
    }

    /// Actuate a command via spawn_blocking (tools are sync).
    async fn actuate_command(&self, cmd: Command) -> Result<Observation, EpisodeFailure> {
        let Command::CallTool {
            call_id,
            name,
            input,
        } = cmd;

        let registry = Arc::clone(&self.registry);
        let name_clone = name.clone();
        let call_id_clone = call_id.clone();

        // spawn_blocking for sync tool execution
        let handle = spawn_blocking(move || match registry.get(&name_clone) {
            None => Observation::Recoverable {
                call_id: Some(call_id_clone),
                error: RecoverableError::UnknownTool(name_clone),
            },
            Some(tool) => match tool.execute(&input) {
                Ok(output) => Observation::ToolResult {
                    call_id: call_id_clone,
                    output,
                },
                Err(e) => Observation::Recoverable {
                    call_id: Some(call_id_clone),
                    error: RecoverableError::ToolFailed {
                        name: name_clone,
                        error: e.to_string(),
                    },
                },
            },
        });

        // Wait for join with cancel select
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                Err(EpisodeFailure::Cancelled)
            }
            result = handle => {
                match result {
                    Ok(obs) => Ok(obs),
                    Err(_join_err) => {
                        // Tool panicked
                        Ok(Observation::Recoverable {
                            call_id: Some(call_id),
                            error: RecoverableError::ToolFailed {
                                name,
                                error: "tool panicked".to_string(),
                            },
                        })
                    }
                }
            }
        }
    }
}

enum EpisodeFailure {
    TaskFatal(TaskFault),
    ServiceFatal(String),
    Cancelled,
}

impl Brainstem {
    /// Spawn the brainstem on a background task and return its JoinHandle.
    /// Helper for tests and examples.
    pub fn spawn(
        mind: Box<dyn Mind>,
        registry: Arc<ToolRegistry>,
        max_steps: usize,
        inbox: mpsc::Receiver<Task>,
        status_rx: mpsc::Receiver<oneshot::Sender<Snapshot>>,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<RunEvent>,
    ) -> tokio::task::JoinHandle<Termination> {
        let brainstem = Brainstem::new(mind, registry, max_steps, inbox, status_rx, cancel, event_tx);
        tokio::spawn(async move { brainstem.run().await })
    }
}
