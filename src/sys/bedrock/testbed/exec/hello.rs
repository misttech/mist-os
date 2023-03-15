// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

fn main() {
    // Read program arguments, skipping the first argument, typically the binary name.
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Include environment variables
    if let Some(animal) = std::env::var("FAVORITE_ANIMAL").ok() {
        args.push(animal);
    }

    // Print a greeting to stdout
    println!("Hello, {}!", greeting(&args));
}

// Return a proper greeting for the list
fn greeting(names: &Vec<String>) -> String {
    // Join the list of names based on length
    match names.len() {
        0 => String::from("Nobody"),
        1 => names.join(""),
        2 => names.join(" and "),
        _ => names.join(", "),
    }
}

#[cfg(test)]
mod tests {
    #[fuchsia::test]
    async fn test_greet_one() {
        let names = vec![String::from("Alice")];
        let expected = "Alice";
        assert_eq!(super::greeting(&names), expected);
    }

    #[fuchsia::test]
    async fn test_greet_two() {
        let names = vec![String::from("Alice"), String::from("Bob")];
        let expected = "Alice and Bob";
        assert_eq!(super::greeting(&names), expected);
    }

    #[fuchsia::test]
    async fn test_greet_three() {
        let names = vec![String::from("Alice"), String::from("Bob"), String::from("Spot")];
        let expected = "Alice, Bob, Spot";
        assert_eq!(super::greeting(&names), expected);
    }
}
