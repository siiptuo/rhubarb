// Copyright (C) 2019 Tuomas Siipola
//
// This file is part of Rhubarb.
//
// Rhubarb is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// Rhubarb program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with Rhubarb.  If not, see <https://www.gnu.org/licenses/>.

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
