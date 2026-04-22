use common::Symbol;
use std::collections::{HashMap, HashSet};
use teloxide::prelude::ChatId;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct SubscriptionManager {
    subs: RwLock<HashMap<Symbol, HashSet<ChatId>>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subs: RwLock::new(HashMap::new()),
        }
    }

    pub async fn add(&self, symbol: Symbol, chat_id: ChatId) {
        let mut map = self.subs.write().await;
        map.entry(symbol).or_default().insert(chat_id);
    }

    pub async fn remove(&self, symbol: &Symbol, chat_id: ChatId) -> bool {
        let mut map = self.subs.write().await;
        if let Some(set) = map.get_mut(symbol) {
            let removed = set.remove(&chat_id);
            if set.is_empty() {
                map.remove(symbol);
            }
            removed
        } else {
            false
        }
    }

    pub async fn get_subscribers(&self, symbol: &Symbol) -> Vec<ChatId> {
        let map = self.subs.read().await;
        map.get(symbol)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    pub async fn get_user_subscriptions(&self, chat_id: ChatId) -> Vec<Symbol> {
        let map = self.subs.read().await;
        map.iter()
            .filter_map(|(sym, set)| set.contains(&chat_id).then_some(sym.clone()))
            .collect()
    }

    pub async fn is_subscribed(&self, symbol: &Symbol, chat_id: ChatId) -> bool {
        let map = self.subs.read().await;
        map.get(symbol)
            .map(|s| s.contains(&chat_id))
            .unwrap_or(false)
    }
}
