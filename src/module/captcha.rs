pub trait CaptchaProvider: Send + Sync {
    fn gen_request(&self);
    fn verify(&self) -> bool;
}
