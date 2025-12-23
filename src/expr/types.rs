pub type PuzzleId = u32;
pub type CountSize = usize;

pub trait PuzzleStates {
    fn is_completed(&self, id: PuzzleId) -> bool;
    fn completed_count(&self) -> CountSize;
    fn completed(&self) -> Vec<PuzzleId>;
    fn game_started(&self) -> bool;
}
