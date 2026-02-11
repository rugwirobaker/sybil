# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Sybil is an application logs storage and querying system built on S3-compatible storage with an SQL query interface.

## Design Philosophy

**S3 as the storage layer**: All persistent data lives in S3-compatible object storage. Compute nodes should be stateless or near-stateless. This keeps storage costs low (important for terabytes of logs) and simplifies operations.

**Minimal components**: Inspired by turbopuffer's architecture - avoid the operational complexity of multi-component systems like ClickHouse clusters. Fewer moving parts to deploy and manage.

**Key technical interests to explore**:
- LSM trees for write-optimized storage
- SIMD for query acceleration
- S3-backed data structures

## Language & Prior Work

**Rust**. Building on patterns from [hedwig](https://github.com/rugwirobaker/hedwig), an S3-backed event delivery system with WAL serialization, checksums, and S3 I/O already solved.

## Tools

Here are some tools at your disposal. They should make some specif worflows easier.

* `html2markdown`: HTML to Markdown. Some websites might be packed with too much useless html and javescript. Pipping the html from curl to `html2markdown` gets us a token friendly version we can add to our context.

## Guidelines

### Before making changes

1. Check the design document (docs/design.md) for architectural context
2. Review existing tests in the affected crate to understand expected behavior
3. Check for existing TODOs: rg "TODO|FIXME|HACK" crates/

## After making changes

1. Hid our lord and savior clippy: Run cargo clippy and fix any complaints it might have.
2. Format the code: with cargo fmt
3. Run tests: make sure tests on affected code still pass and expand with more tests if necessary.

## Inspirations

- [turbopuffer](https://turbopuffer.com/blog/turbopuffer) - S3-native vector database, simple architecture
- [WarpStream](https://www.warpstream.com/) - S3-backed Kafka alternative
- [Aiven diskless Kafka](https://aiven.io/blog/guide-diskless-apache-kafka-kip-1150) - KIP-1150
- [Fly.io S3 filesystems](https://fly.io/blog/design-and-implementation/) - S3-backed filesystems for agent sandboxes
- [Logchef](https://logchef.app/) - Query interface reference (uses ClickHouse)
