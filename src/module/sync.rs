use std::collections::HashMap;

use actix::{Actor, Context};

#[derive(Default)]
pub struct SyncHub {
    users: HashMap<i32, usize>,
}


impl Actor for SyncHub {
    type Context = Context<Self>;
}
