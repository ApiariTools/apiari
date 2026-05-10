// Comprehensive tests for apiari-workflow.
//
// Each test module covers a distinct engine behaviour so failures are easy to isolate.

mod helpers;

mod test_compensation;
mod test_context;
mod test_coverage_gaps;
mod test_cycle_detection;
mod test_db;
mod test_edge_cases;
mod test_goto;
mod test_integration;
mod test_linear;
mod test_registry;
mod test_retry;
mod test_signals;
mod test_timeout;
mod test_timer;
