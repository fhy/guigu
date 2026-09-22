use async_trait::async_trait;
use futures::stream;
use guigu::core::provider::{
    AssistantEvent, AssistantStream, ModelProvider, ProviderError, ProviderRequest,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

pub struct FakeProvider {
    pub turns: Vec<Vec<AssistantEvent>>,
    pub call_index: AtomicUsize,
    pub call_count: AtomicUsize,
    pub fail_next: AtomicUsize,
    pub scripted_errors: Mutex<VecDeque<ProviderError>>,
    pub last_context_size: AtomicUsize,
    pub gate: Mutex<Option<oneshot::Receiver<()>>>,
}
impl FakeProvider {
    pub fn new(turns: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Self::with(turns, 0, None)
    }
    pub fn with(
        turns: Vec<Vec<AssistantEvent>>,
        fail_next: usize,
        gate: Option<oneshot::Receiver<()>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            turns,
            call_index: AtomicUsize::new(0),
            call_count: AtomicUsize::new(0),
            fail_next: AtomicUsize::new(fail_next),
            scripted_errors: Mutex::new(VecDeque::new()),
            last_context_size: AtomicUsize::new(0),
            gate: Mutex::new(gate),
        })
    }
    pub fn with_errors(turns: Vec<Vec<AssistantEvent>>, errors: Vec<ProviderError>) -> Arc<Self> {
        let provider = Self::new(turns);
        *provider.scripted_errors.lock().expect("error mutex") = errors.into();
        provider
    }
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
    pub fn last_context_size(&self) -> usize {
        self.last_context_size.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl ModelProvider for FakeProvider {
    async fn stream(&self, request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let rx = self.gate.lock().expect("gate mutex").take();
        if let Some(rx) = rx {
            let _ = rx.await;
        }
        let remaining = self.fail_next.load(Ordering::SeqCst);
        if remaining > 0 {
            self.fail_next.fetch_sub(1, Ordering::SeqCst);
            return Err(ProviderError::Request(
                "simulated establishment failure".to_string(),
            ));
        }
        if let Some(error) = self
            .scripted_errors
            .lock()
            .expect("error mutex")
            .pop_front()
        {
            return Err(error);
        }
        self.last_context_size
            .store(request.context.messages.len(), Ordering::SeqCst);
        let idx = self.call_index.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(
            self.turns.get(idx).cloned().unwrap_or_default(),
        )))
    }
}

pub struct HangingProvider {
    pub call_count: AtomicUsize,
}
impl HangingProvider {
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl ModelProvider for HangingProvider {
    async fn stream(&self, _request: ProviderRequest) -> Result<AssistantStream, ProviderError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        futures::future::pending::<()>().await;
        Err(ProviderError::Request("unreachable".to_string()))
    }
}
