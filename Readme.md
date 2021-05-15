# Async events

Waiting for external task completion in asynchronous Rust code.

## Motivation

A pair of Future and data structure originally developed for the [throttle semaphore sever](https://github.com/pacman82/throttle), to handle a large amount of blocking request while waiting for notification from external services that semaphores have been freed again. It occurred to me that this code might also be useful to other services waiting on external events, not driving the futures to completion within their own process.

## Usage

This crate is independent of the asynchronous runtime used (e.g. `tokio`).

See `https://docs.rs/async-events` for documentation.
