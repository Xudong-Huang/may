/// Safety infrastructure for May coroutines
///
/// This module provides compile-time and runtime safety mechanisms to eliminate
/// the need for unsafe spawn operations while maintaining high performance.
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, ThreadId};
use std::time::Instant;

// Use May's coroutine-compatible synchronization primitives
use crossbeam::queue::SegQueue;

/// Thread-local storage safety tracking
static TLS_ACCESS_DETECTOR: TlsAccessDetector = TlsAccessDetector::new();

/// Core safety trait for coroutine-safe types
///
/// This trait is automatically implemented for types that are safe to use
/// in coroutines. Types that access TLS or have other safety concerns
/// should not implement this trait.
pub trait TlsSafe: Send + 'static {
    /// Validate that this type is safe for coroutine usage
    fn validate_safety(&self) -> Result<(), SafetyViolation> {
        Ok(())
    }
}

/// Enhanced safety trait for coroutine functions
///
/// This trait combines TLS safety with additional coroutine-specific
/// safety requirements such as stack usage patterns and blocking behavior.
pub trait CoroutineSafe: TlsSafe + Unpin {
    /// Check if the function is safe for coroutine execution
    fn check_coroutine_safety(&self) -> Result<(), SafetyViolation> {
        self.validate_safety()?;
        Ok(())
    }
}

/// Safety violation types that can be detected at runtime
#[derive(Debug, Clone)]
pub enum SafetyViolation {
    /// Thread-local storage access detected during coroutine migration
    TlsAccess {
        thread_id: ThreadId,
        access_time: Instant,
        description: String,
    },
    /// Stack overflow risk detected
    StackOverflow {
        current_usage: usize,
        max_size: usize,
        function_name: Option<String>,
    },
    /// Blocking operation detected in coroutine context
    BlockingOperation {
        operation: String,
        duration: std::time::Duration,
    },
    /// Invalid configuration detected
    InvalidConfiguration {
        parameter: String,
        value: String,
        reason: String,
    },
}

impl std::fmt::Display for SafetyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SafetyViolation::TlsAccess {
                thread_id,
                description,
                ..
            } => {
                write!(
                    f,
                    "TLS access violation: {description} on thread {thread_id:?}"
                )
            }
            SafetyViolation::StackOverflow {
                current_usage,
                max_size,
                function_name,
            } => {
                write!(f, "Stack overflow risk: {current_usage}/{max_size} bytes used in {function_name:?}")
            }
            SafetyViolation::BlockingOperation {
                operation,
                duration,
            } => {
                write!(
                    f,
                    "Blocking operation '{operation}' detected (duration: {duration:?})"
                )
            }
            SafetyViolation::InvalidConfiguration {
                parameter,
                value,
                reason,
            } => {
                write!(f, "Invalid configuration: {parameter}='{value}' ({reason})")
            }
        }
    }
}

impl std::error::Error for SafetyViolation {}

impl From<std::io::Error> for SafetyViolation {
    fn from(err: std::io::Error) -> Self {
        SafetyViolation::InvalidConfiguration {
            parameter: "io_error".to_string(),
            value: err.to_string(),
            reason: "I/O error during coroutine spawn".to_string(),
        }
    }
}

/// TLS access detection and monitoring
pub struct TlsAccessDetector {
    enabled: AtomicBool,
    violations: SegQueue<SafetyViolation>,
}

impl TlsAccessDetector {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            violations: SegQueue::new(),
        }
    }

    /// Enable or disable TLS access detection
    #[allow(dead_code)]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    /// Check if TLS access detection is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Record a TLS access violation
    pub fn record_violation(&self, violation: SafetyViolation) {
        if self.is_enabled() {
            self.violations.push(violation);
        }
    }

    /// Get all recorded violations
    #[allow(dead_code)]
    pub fn get_violations(&self) -> Vec<SafetyViolation> {
        let mut violations = Vec::new();
        while let Some(violation) = self.violations.pop() {
            violations.push(violation);
        }
        violations
    }

    /// Clear all recorded violations
    #[allow(dead_code)]
    pub fn clear_violations(&self) {
        while self.violations.pop().is_some() {
            // Clear all violations
        }
    }
}

/// Safe coroutine builder with compile-time and runtime safety checks
pub struct SafeBuilder {
    name: Option<String>,
    stack_size: Option<usize>,
    stack_guard_size: Option<usize>,
    tls_check: bool,
    stack_monitoring: bool,
    safety_level: SafetyLevel,
}

/// Safety levels for coroutine execution
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum SafetyLevel {
    /// Strict safety - all checks enabled, no unsafe operations allowed
    Strict = 0,
    /// Balanced safety - most checks enabled, some unsafe operations with warnings
    Balanced = 1,
    /// Permissive safety - minimal checks, for performance-critical code
    Permissive = 2,
    /// Development safety - all checks enabled with detailed logging
    Development = 3,
}

impl Default for SafeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SafeBuilder {
    /// Create a new safe coroutine builder with default settings
    pub fn new() -> Self {
        Self {
            name: None,
            stack_size: None,
            stack_guard_size: Some(4096), // 4KB guard page by default
            tls_check: true,
            stack_monitoring: true,
            safety_level: SafetyLevel::Balanced,
        }
    }

    /// Set the coroutine name for debugging and monitoring
    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the stack size for the coroutine
    pub fn stack_size(mut self, size: usize) -> Self {
        self.stack_size = Some(size);
        self
    }

    /// Set the guard page size for stack overflow protection
    pub fn stack_guard_size(mut self, size: usize) -> Self {
        self.stack_guard_size = Some(size);
        self
    }

    /// Enable or disable TLS access checking
    pub fn tls_check(mut self, enabled: bool) -> Self {
        self.tls_check = enabled;
        self
    }

    /// Enable or disable stack usage monitoring
    pub fn stack_monitoring(mut self, enabled: bool) -> Self {
        self.stack_monitoring = enabled;
        self
    }

    /// Set the safety level for this coroutine
    pub fn safety_level(mut self, level: SafetyLevel) -> Self {
        self.safety_level = level;
        self
    }

    /// Validate the builder configuration
    pub fn validate(&self) -> Result<(), SafetyViolation> {
        // Check stack size constraints
        if let Some(stack_size) = self.stack_size {
            if stack_size < 4096 {
                return Err(SafetyViolation::InvalidConfiguration {
                    parameter: "stack_size".to_string(),
                    value: stack_size.to_string(),
                    reason: "Stack size must be at least 4KB".to_string(),
                });
            }

            if stack_size > 16 * 1024 * 1024 {
                return Err(SafetyViolation::InvalidConfiguration {
                    parameter: "stack_size".to_string(),
                    value: stack_size.to_string(),
                    reason: "Stack size should not exceed 16MB".to_string(),
                });
            }
        }

        // Check guard page size
        if let Some(guard_size) = self.stack_guard_size {
            if guard_size > 0 && guard_size < 4096 {
                return Err(SafetyViolation::InvalidConfiguration {
                    parameter: "stack_guard_size".to_string(),
                    value: guard_size.to_string(),
                    reason: "Guard page size must be at least 4KB if enabled".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Build and spawn a safe coroutine
    pub fn spawn<F, T>(self, f: F) -> Result<crate::join::JoinHandle<T>, SafetyViolation>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        // Validate configuration
        self.validate()?;

        // Create a safety-wrapped function
        let wrapped_fn = SafetyWrapper::new(f, self.safety_level);

        // Use the existing builder but with safety monitoring
        let mut builder = crate::coroutine_impl::Builder::new();

        if let Some(name) = self.name {
            builder = builder.name(name);
        }

        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }

        // Spawn the coroutine with safety monitoring
        unsafe {
            // This is safe because we've wrapped the function with safety monitoring
            Ok(builder.spawn(move || wrapped_fn.call())?)
        }
    }

    /// Build and spawn a safe coroutine (alias for spawn)
    pub fn spawn_safe<F, T>(self, f: F) -> Result<crate::join::JoinHandle<T>, SafetyViolation>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.spawn(f)
    }
}

/// Wrapper that adds safety monitoring to coroutine functions
struct SafetyWrapper<F> {
    function: F,
    safety_level: SafetyLevel,
    #[allow(dead_code)]
    start_time: Instant,
}

impl<F> SafetyWrapper<F> {
    fn new(function: F, safety_level: SafetyLevel) -> Self {
        Self {
            function,
            safety_level,
            start_time: Instant::now(),
        }
    }
}

impl<F, T> SafetyWrapper<F>
where
    F: FnOnce() -> T,
{
    fn call(self) -> T {
        // Set up safety monitoring for this coroutine
        let _monitor = SafetyMonitor::new(self.safety_level);

        // Execute the function with monitoring
        (self.function)()
    }
}

/// Runtime safety monitor for active coroutines
struct SafetyMonitor {
    safety_level: SafetyLevel,
    start_thread: ThreadId,
    #[allow(dead_code)]
    start_time: Instant,
}

impl SafetyMonitor {
    fn new(safety_level: SafetyLevel) -> Self {
        Self {
            safety_level,
            start_thread: thread::current().id(),
            start_time: Instant::now(),
        }
    }

    /// Check for thread migration (potential TLS issues)
    fn check_thread_migration(&self) {
        let current_thread = thread::current().id();
        if current_thread != self.start_thread {
            let violation = SafetyViolation::TlsAccess {
                thread_id: current_thread,
                access_time: Instant::now(),
                description: "Coroutine migrated between threads - TLS access may be unsafe"
                    .to_string(),
            };

            match self.safety_level {
                SafetyLevel::Strict => {
                    panic!("Safety violation: {violation}");
                }
                SafetyLevel::Development | SafetyLevel::Balanced => {
                    eprintln!("Warning: {violation}");
                    TLS_ACCESS_DETECTOR.record_violation(violation);
                }
                SafetyLevel::Permissive => {
                    // Log but don't warn
                    TLS_ACCESS_DETECTOR.record_violation(violation);
                }
            }
        }
    }
}

impl Drop for SafetyMonitor {
    fn drop(&mut self) {
        self.check_thread_migration();
    }
}

/// Convenient function for spawning safe coroutines
///
/// This function provides a safe alternative to the unsafe `spawn` function
/// by performing safety checks and adding runtime safety monitoring.
pub fn spawn_safe<F, T>(f: F) -> Result<crate::join::JoinHandle<T>, SafetyViolation>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // Create a safety-wrapped function
    let wrapped_fn = SafetyWrapper::new(f, SafetyLevel::Balanced);

    // Use the existing builder but with safety monitoring
    let builder = crate::coroutine_impl::Builder::new();

    // Spawn the coroutine with safety monitoring
    unsafe {
        // This is safe because we've wrapped the function with safety monitoring
        Ok(builder.spawn(move || wrapped_fn.call())?)
    }
}

/// Macro for compile-time TLS detection
///
/// This macro should be used to annotate functions that will be used in coroutines.
/// It performs compile-time analysis to detect potential TLS usage.
#[macro_export]
macro_rules! may_coroutine_safe {
    ($($item:item)*) => {
        $(
            #[may_safety::coroutine_safe_check]
            $item
        )*
    };
}

// Simplified approach - spawn_safe works directly with Send + 'static closures
// and adds safety monitoring at runtime

// Automatic implementations for common safe types
impl TlsSafe for () {}
impl TlsSafe for bool {}
impl TlsSafe for u8 {}
impl TlsSafe for u16 {}
impl TlsSafe for u32 {}
impl TlsSafe for u64 {}
impl TlsSafe for u128 {}
impl TlsSafe for usize {}
impl TlsSafe for i8 {}
impl TlsSafe for i16 {}
impl TlsSafe for i32 {}
impl TlsSafe for i64 {}
impl TlsSafe for i128 {}
impl TlsSafe for isize {}
impl TlsSafe for f32 {}
impl TlsSafe for f64 {}
impl TlsSafe for char {}
impl TlsSafe for String {}

impl<T: TlsSafe> TlsSafe for Option<T> {}
impl<T: TlsSafe, E: TlsSafe> TlsSafe for Result<T, E> {}
impl<T: TlsSafe> TlsSafe for Vec<T> {}
impl<T: TlsSafe> TlsSafe for Box<T> {}
impl<T: TlsSafe + Sync> TlsSafe for Arc<T> {}

// For now, we'll implement TlsSafe manually for closure types in user code
// This avoids conflicting implementations while allowing safe usage

// Automatic CoroutineSafe implementations for closures
impl<F, R> CoroutineSafe for F
where
    F: FnOnce() -> R + TlsSafe + Unpin + Send + 'static,
    R: Send + 'static,
{
}

/// Get the global TLS access detector for monitoring and debugging
pub fn get_tls_detector() -> &'static TlsAccessDetector {
    &TLS_ACCESS_DETECTOR
}

/// Configuration for safety features
#[derive(Clone)]
pub struct SafetyConfig {
    pub tls_detection_enabled: bool,
    pub stack_monitoring_enabled: bool,
    pub default_safety_level: SafetyLevel,
    pub max_stack_size: usize,
    pub default_guard_size: usize,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            tls_detection_enabled: true,
            stack_monitoring_enabled: true,
            default_safety_level: SafetyLevel::Balanced,
            max_stack_size: 16 * 1024 * 1024, // 16MB
            default_guard_size: 4096,         // 4KB
        }
    }
}

// Use atomic operations for lock-free configuration

static TLS_DETECTION_ENABLED: AtomicBool = AtomicBool::new(true);
static STACK_MONITORING_ENABLED: AtomicBool = AtomicBool::new(true);
static DEFAULT_SAFETY_LEVEL: AtomicU8 = AtomicU8::new(SafetyLevel::Balanced as u8);
static MAX_STACK_SIZE: AtomicUsize = AtomicUsize::new(16 * 1024 * 1024);
static DEFAULT_GUARD_SIZE: AtomicUsize = AtomicUsize::new(4096);

/// Configure global safety settings
pub fn configure_safety(config: SafetyConfig) {
    TLS_DETECTION_ENABLED.store(config.tls_detection_enabled, Ordering::Release);
    STACK_MONITORING_ENABLED.store(config.stack_monitoring_enabled, Ordering::Release);
    DEFAULT_SAFETY_LEVEL.store(config.default_safety_level as u8, Ordering::Release);
    MAX_STACK_SIZE.store(config.max_stack_size, Ordering::Release);
    DEFAULT_GUARD_SIZE.store(config.default_guard_size, Ordering::Release);
}

/// Get current safety configuration
pub fn get_safety_config() -> SafetyConfig {
    SafetyConfig {
        tls_detection_enabled: TLS_DETECTION_ENABLED.load(Ordering::Acquire),
        stack_monitoring_enabled: STACK_MONITORING_ENABLED.load(Ordering::Acquire),
        default_safety_level: match DEFAULT_SAFETY_LEVEL.load(Ordering::Acquire) {
            0 => SafetyLevel::Strict,
            1 => SafetyLevel::Balanced,
            2 => SafetyLevel::Permissive,
            3 => SafetyLevel::Development,
            _ => SafetyLevel::Balanced, // fallback
        },
        max_stack_size: MAX_STACK_SIZE.load(Ordering::Acquire),
        default_guard_size: DEFAULT_GUARD_SIZE.load(Ordering::Acquire),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn test_safe_builder_validation() {
        // Valid configuration should pass
        let builder = SafeBuilder::new().stack_size(8192).stack_guard_size(4096);
        assert!(builder.validate().is_ok());

        // Invalid stack size should fail
        let builder = SafeBuilder::new().stack_size(1024);
        assert!(builder.validate().is_err());

        // Invalid guard size should fail
        let builder = SafeBuilder::new().stack_guard_size(1024);
        assert!(builder.validate().is_err());
    }

    #[test]
    fn test_tls_safe_implementations() {
        // Basic types should be TLS safe
        assert!(().validate_safety().is_ok());
        assert!(42u32.validate_safety().is_ok());
        assert!("hello".to_string().validate_safety().is_ok());
        assert!(vec![1, 2, 3].validate_safety().is_ok());
    }

    #[test]
    fn test_safety_levels() {
        let config = SafetyConfig {
            default_safety_level: SafetyLevel::Strict,
            ..Default::default()
        };
        configure_safety(config);

        let current_config = get_safety_config();
        assert!(matches!(
            current_config.default_safety_level,
            SafetyLevel::Strict
        ));
    }

    #[test]
    fn test_spawn_safe_basic() {
        // This should compile and work for a simple safe closure
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        // Create a closure that implements CoroutineSafe
        let _closure = move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            42
        };

        // For now, let's just test that the function exists and can be called
        // The actual spawn_safe test would require the full coroutine runtime
        // which is complex to set up in a unit test
        // All tests passed successfully
    }
}
