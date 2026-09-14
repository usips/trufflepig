//! Token refill is available to callers and tests.
pub struct TokenBucket { pub available: usize }
impl TokenBucket {
    pub fn refill_tokens(&mut self, amount: usize) {
        let replenished = self.available + amount;
        self.available = replenished;
    }
}
fn direct_helper(value: usize) -> usize { value + 1 }
pub fn call_helper() -> usize { direct_helper(2) }
trait Counter { fn count(&self) -> usize; }
impl Counter for TokenBucket { fn count(&self) -> usize { self.available } }
#[cfg(feature = "fast")]
pub fn configured_limit() -> usize { 99 }
#[cfg(not(feature = "fast"))]
pub fn configured_limit() -> usize { 10 }
