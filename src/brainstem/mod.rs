//! Brainstem: the body + runtime for the actor agent (spec 002 goals 6-13).

use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::spawn_blocking;
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;

use crate::budget::BudgetSummary;
use crate::event::{RunEvent, Termination};
use crate::mind::{Command, Decision, Mind, Perception, Reason, TaskFault};
use crate::observation::{Observation, Outcome, RecoverableError, TaskOutcome};
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
    /// Set while throttling: the instant the brainstem will wake. The reset that
    /// the agent is actually waiting on (goal 12 reports the next reset instant).
    throttle_wake: Option<Instant>,
}

impl BrainstemState {
    fn build_snapshot(&self, queue_depth: usize) -> Snapshot {
        // While throttling, the reset the agent is waiting on is the throttle
        // wake instant, not the (stale) summary captured before the decide.
        let next_reset = match (self.lifecycle, self.throttle_wake) {
            (Lifecycle::Throttling, Some(wake)) => wake,
            _ => self.budget_summary.next_reset,
        };
        Snapshot {
            lifecycle: self.lifecycle,
            current_task: self.current_task.clone(),
            tokens_remaining: self.budget_summary.tokens_remaining,
            next_reset,
            queue_depth,
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
        // Share the brainstem's single event stream with the mind so cognitive
        // events (RetryScheduled, WindowReset) interleave with brainstem events
        // (plan 002: both ends are producers on one channel).
        self.mind.set_event_sink(self.event_tx.clone());
        self.mind.set_tools(self.registry.schemas());

        let mut state = BrainstemState {
            lifecycle: Lifecycle::Idle,
            current_task: None,
            budget_summary: self.mind.budget_summary(),
            steps_used: 0,
            throttle_wake: None,
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
                    let snapshot = state.build_snapshot(self.inbox.len());
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
                            let snapshot = state.build_snapshot(self.inbox.len());
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
                    let obs = self.actuate_command(cmd, state).await?;

                    let ok = matches!(obs, Observation::ToolResult { .. });
                    let call_id = match &obs {
                        Observation::ToolResult { call_id, .. } => call_id.clone(),
                        Observation::Recoverable { call_id, .. } => {
                            call_id.clone().unwrap_or_default()
                        }
                    };

                    let _ = self.event_tx.send(RunEvent::CommandResult { call_id, ok });

                    if let Observation::Recoverable { error, .. } = &obs {
                        // `RecoverableObservation` is the v0.2 canonical recoverable
                        // event (no `step` — the perpetual loop has no 001 step model).
                        // The 001 `Recovered` variant is left to the 001 loop; emit one
                        // event per recoverable observation, not two.
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
                    state.throttle_wake = Some(wake);
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
                                let snapshot = state.build_snapshot(self.inbox.len());
                                let _ = reply_tx.send(snapshot);
                            }
                            _ = &mut sleep_fut => {
                                break;
                            }
                        }
                    }

                    state.lifecycle = Lifecycle::Working;
                    state.throttle_wake = None;
                    perception = Perception::Resume;
                }
            }
        }
    }

    /// Actuate a command via spawn_blocking (tools are sync). Services cancellation
    /// and Status queries concurrently while the tool runs (spec goal 11).
    async fn actuate_command(
        &mut self,
        cmd: Command,
        state: &BrainstemState,
    ) -> Result<Observation, EpisodeFailure> {
        let Command::CallTool {
            call_id,
            name,
            input,
        } = cmd;

        let registry = Arc::clone(&self.registry);
        let name_clone = name.clone();
        let call_id_clone = call_id.clone();

        // spawn_blocking for sync tool execution
        let mut handle = spawn_blocking(move || match registry.get(&name_clone) {
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

        // Wait for join, servicing cancellation and Status queries concurrently so a
        // long-running tool cannot block either (spec goal 11). `&mut handle` keeps
        // the tool alive across Status replies (JoinHandle is Unpin).
        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    return Err(EpisodeFailure::Cancelled);
                }
                Some(reply_tx) = self.status_rx.recv() => {
                    let _ = reply_tx.send(state.build_snapshot(self.inbox.len()));
                }
                result = &mut handle => {
                    return match result {
                        Ok(obs) => Ok(obs),
                        // Tool panicked
                        Err(_join_err) => Ok(Observation::Recoverable {
                            call_id: Some(call_id),
                            error: RecoverableError::ToolFailed {
                                name,
                                error: "tool panicked".to_string(),
                            },
                        }),
                    };
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
        let brainstem = Brainstem::new(
            mind, registry, max_steps, inbox, status_rx, cancel, event_tx,
        );
        tokio::spawn(async move { brainstem.run().await })
    }
}
