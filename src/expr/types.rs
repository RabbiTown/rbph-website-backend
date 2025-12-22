pub type PuzzleId = u32;
pub type CountSize = usize;

pub trait PuzzleStates {
    fn is_unlocked(&self, id: PuzzleId) -> bool;
    fn unlocked_count(&self) -> CountSize;
    fn unlocked() -> Vec<PuzzleId>;
}
