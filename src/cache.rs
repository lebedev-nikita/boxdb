use crate::Result;
use sqlparser::ast::Statement;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

#[derive(Default)]
pub(crate) struct ParseCache {
    statements: HashMap<String, Arc<Vec<Statement>>>,
    order: VecDeque<String>,
    bytes: usize,
}
impl ParseCache {
    pub fn get(&mut self, sql: &str) -> Result<Arc<Vec<Statement>>> {
        if let Some(statements) = self.statements.get(sql) {
            return Ok(statements.clone());
        }
        let statements = Arc::new(crate::parse(sql)?);
        // Bound both entry count and source size; large one-off imports are not cached.
        if sql.len() > 64 * 1024 {
            return Ok(statements);
        }
        while self.statements.len() >= 512 || self.bytes + sql.len() > 1024 * 1024 {
            let key = self.order.pop_front().unwrap();
            self.bytes -= key.len();
            self.statements.remove(&key);
        }
        self.bytes += sql.len();
        self.order.push_back(sql.into());
        self.statements.insert(sql.into(), statements.clone());
        Ok(statements)
    }
}
