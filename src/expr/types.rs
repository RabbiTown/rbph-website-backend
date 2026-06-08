pub type PuzzleId = u32;
pub type RoundId = u32;
pub type CountSize = usize;

#[derive(Debug, Clone)]
pub enum PuzzleRef {
    Id(PuzzleId),
    Slug(String),
}

#[derive(Debug, Clone)]
pub enum RoundRef {
    Id(RoundId),
    Slug(String),
}

pub trait PuzzleStates {
    fn is_solved(&self, id: PuzzleId) -> bool;
    fn solved(&self) -> Vec<PuzzleId>;
    fn puzzle_slug(&self, slug: &str) -> Option<PuzzleId>;
    fn round_slug(&self, slug: &str) -> Option<RoundId>;
    fn round_puzzles(&self, id: RoundId) -> Option<Vec<PuzzleId>>;
    fn game_started(&self) -> bool;
}
