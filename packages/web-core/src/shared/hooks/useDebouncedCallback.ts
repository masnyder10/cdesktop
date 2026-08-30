import { useRef, useEffect } from 'react';

/**
 * Returns a debounced version of the callback that delays invocation
 * until after `delay` milliseconds have elapsed since the last call.
 * Also returns `cancel` to drop a pending invocation and `flush` to run it now.
 */
export function useDebouncedCallback<Args extends unknown[]>(
  callback: (...args: Args) => void,
  delay: number
): {
  debounced: (...args: Args) => void;
  cancel: () => void;
  flush: () => void;
} {
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const callbackRef = useRef(callback);
  // Latest args seen, kept so flush() can run the pending call immediately.
  const pendingArgsRef = useRef<Args | null>(null);

  // Keep callback ref up to date
  useEffect(() => {
    callbackRef.current = callback;
  }, [callback]);

  // Return stable function references
  const debouncedRef = useRef((...args: Args) => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
    }
    pendingArgsRef.current = args;
    timeoutRef.current = setTimeout(() => {
      timeoutRef.current = null;
      const pending = pendingArgsRef.current;
      pendingArgsRef.current = null;
      if (pending) callbackRef.current(...pending);
    }, delay);
  });

  // Cancel function to clear pending timeout
  const cancelRef = useRef(() => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    pendingArgsRef.current = null;
  });

  // Flush: run any pending invocation immediately. Used so a draft isn't lost
  // when the component unmounts or the window is closing.
  const flushRef = useRef(() => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    const pending = pendingArgsRef.current;
    pendingArgsRef.current = null;
    if (pending) callbackRef.current(...pending);
  });

  // Flush on unmount rather than dropping the pending call. Switching sessions
  // or closing otherwise discarded the last <delay>ms of typing.
  useEffect(() => {
    const flush = flushRef.current;
    return () => flush();
  }, []);

  return {
    debounced: debouncedRef.current,
    cancel: cancelRef.current,
    flush: flushRef.current,
  };
}
