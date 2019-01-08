pub struct Challenge(String);

impl Challenge {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

pub trait Generator {
    fn generate(self) -> Challenge;
}

pub mod generators {
    use super::*;
    use rand::{distributions::Alphanumeric, thread_rng, Rng};

    pub struct Random;

    impl Random {
        pub fn new() -> Self {
            Self {}
        }
    }

    impl Generator for Random {
        fn generate(self) -> Challenge {
            Challenge(thread_rng().sample_iter(&Alphanumeric).take(32).collect())
        }
    }

    pub struct Static {
        challenge: String,
    }

    impl Static {
        pub fn new(challenge: String) -> Self {
            Self { challenge }
        }
    }

    impl Generator for Static {
        fn generate(self) -> Challenge {
            Challenge(self.challenge)
        }
    }
}
