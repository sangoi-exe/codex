use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotification;
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::BufReader;
use std::process::Stdio;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Debug, Serialize, Clone)]
pub struct AuxAgentSummary {
    pub agent_id: Uuid,
    pub running: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct SpawnAuxAgentResult {
    pub agent_id: Uuid,
}

struct AuxAgentState {
    child: Arc<Mutex<Child>>,
    _completion_task: JoinHandle<()>,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

struct AuxAgentManagerInner {
    max_agents: usize,
    current_exe: PathBuf,
    default_cwd: PathBuf,
    outgoing: Arc<OutgoingMessageSender>,
    agents: Mutex<HashMap<Uuid, AuxAgentState>>,
}

impl AuxAgentManagerInner {
    fn new(
        max_agents: usize,
        current_exe: PathBuf,
        default_cwd: PathBuf,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> Self {
        Self {
            max_agents,
            current_exe,
            default_cwd,
            outgoing,
            agents: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Clone)]
pub struct AuxAgentManager {
    inner: Arc<AuxAgentManagerInner>,
}

impl AuxAgentManager {
    pub fn new(
        max_agents: usize,
        current_exe: PathBuf,
        default_cwd: PathBuf,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> Self {
        Self {
            inner: Arc::new(AuxAgentManagerInner::new(
                max_agents,
                current_exe,
                default_cwd,
                outgoing,
            )),
        }
    }

    pub async fn spawn_agent(
        &self,
        prompt: String,
        cwd: Option<PathBuf>,
    ) -> Result<SpawnAuxAgentResult, io::Error> {
        {
            let map = self.inner.agents.lock().await;
            if map.len() >= self.inner.max_agents {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "maximum number of auxiliary agents reached",
                ));
            }
        }

        let mut command = Command::new(&self.inner.current_exe);
        command
            .arg("exec")
            .arg("--skip-git-repo-check")
            .arg("--ask-for-approval")
            .arg("never")
            .arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command.current_dir(cwd.unwrap_or_else(|| self.inner.default_cwd.clone()));

        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        let agent_id = Uuid::new_v4();

        let stdout_task = stdout.map(|stream| self.spawn_output_task(agent_id, stream, "stdout"));
        let stderr_task = stderr.map(|stream| self.spawn_output_task(agent_id, stream, "stderr"));

        let completion_child = child.clone();
        let outgoing = self.inner.outgoing.clone();
        let manager = self.clone();
        let completion_task = tokio::spawn(async move {
            if let Ok(status) = completion_child.lock().await.wait().await {
                let payload = serde_json::json!({
                    "agent_id": agent_id,
                    "status": status.code(),
                });
                outgoing
                    .send_notification(OutgoingNotification {
                        method: "codex/aux-agent/exit".to_string(),
                        params: Some(payload),
                    })
                    .await;
            }
            manager.remove_agent(agent_id).await;
        });

        let mut map = self.inner.agents.lock().await;
        map.insert(
            agent_id,
            AuxAgentState {
                child,
                _completion_task: completion_task,
                stdout_task,
                stderr_task,
            },
        );

        Ok(SpawnAuxAgentResult { agent_id })
    }

    pub async fn stop_agent(&self, agent_id: Uuid) -> Result<(), io::Error> {
        let map = self.inner.agents.lock().await;
        if let Some(state) = map.get(&agent_id) {
            let mut child = state.child.lock().await;
            child.kill().await?;
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "auxiliary agent not found",
            ))
        }
    }

    pub async fn list_agents(&self) -> Vec<AuxAgentSummary> {
        let map = self.inner.agents.lock().await;
        let entries: Vec<(Uuid, Arc<Mutex<Child>>)> = map
            .iter()
            .map(|(id, state)| (*id, state.child.clone()))
            .collect();
        drop(map);

        let mut summaries = Vec::new();
        for (id, child) in entries {
            let running = match child.lock().await.try_wait() {
                Ok(Some(_)) => false,
                Ok(None) => true,
                Err(_) => false,
            };
            summaries.push(AuxAgentSummary {
                agent_id: id,
                running,
            });
        }
        summaries
    }

    async fn remove_agent(&self, agent_id: Uuid) {
        let mut map = self.inner.agents.lock().await;
        if let Some(mut state) = map.remove(&agent_id) {
            if let Some(task) = state.stdout_task.take() {
                task.abort();
            }
            if let Some(task) = state.stderr_task.take() {
                task.abort();
            }
        }
    }

    fn spawn_output_task<R>(
        &self,
        agent_id: Uuid,
        stream: R,
        stream_label: &'static str,
    ) -> JoinHandle<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let outgoing = self.inner.outgoing.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let payload = serde_json::json!({
                            "agent_id": agent_id,
                            "stream": stream_label,
                            "line": line.trim_end_matches('\n'),
                        });
                        outgoing
                            .send_notification(OutgoingNotification {
                                method: "codex/aux-agent/output".to_string(),
                                params: Some(payload),
                            })
                            .await;
                    }
                    Err(_) => break,
                }
            }
        })
    }
}
