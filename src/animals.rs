use std::sync::LazyLock;

use rand::{rngs::OsRng, seq::SliceRandom};

pub fn get_animal_name() -> &'static str {
    static ANIMALS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let animals = include_str!("./animals.txt");
        animals.lines().collect()
    });

    ANIMALS
        .choose(&mut OsRng)
        .expect("List should not be empty")
}
