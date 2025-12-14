pub type PluzzeId = u32;
pub type CountSize = usize;

pub trait PluzzesState {
    fn is_unlocked(&self, id: PluzzeId) -> bool;
    fn unlocked_count(&self) -> CountSize;
    fn unlocked() -> Vec<PluzzeId>;
}
