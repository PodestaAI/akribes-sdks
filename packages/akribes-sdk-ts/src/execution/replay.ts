/**
 * Replay stored EngineEvent[] arrays into ExecutionStep[] for display.
 *
 * Uses the same reducer as the live handler but processes events in bulk
 * (no incremental state). Also exposes a step-through controller for
 * historical debugging.
 */
import type { EngineEvent, HubEvent } from '../types';
import {
  reduceExecutionEvent,
  type ExecutionStep,
  type ActiveLineRef,
  type ActiveNodeRef,
} from './steps';

/** Apply the reducer to every event in order and return the resulting steps. */
export function replayEvents(events: EngineEvent[]): ExecutionStep[] {
  let steps: ExecutionStep[] = [];
  const activeLineRef: ActiveLineRef = { current: null };
  const activeNodeRef: ActiveNodeRef = { current: null };

  for (const evt of events) {
    const hubEvt: HubEvent = {
      type: 'Execution',
      payload: { project_id: 0, script_name: '', execution_id: '', event: evt },
    };
    const result = reduceExecutionEvent(steps, hubEvt, activeLineRef, activeNodeRef);
    steps = result.steps;
  }

  return steps;
}

/**
 * Step-through replay controller for historical executions. Allows stepping
 * forward/backward through events one at a time, seeking to arbitrary
 * positions, and getting the current active line.
 */
export type ReplayController = {
  eventIndex: number;
  totalEvents: number;
  steps: ExecutionStep[];
  activeLine: number | null;
  stepForward(): ReplayState;
  stepBackward(): ReplayState;
  seekTo(index: number): ReplayState;
};

export type ReplayState = {
  steps: ExecutionStep[];
  activeLine: number | null;
  eventIndex: number;
  done: boolean;
};

/**
 * Create a replay controller from a list of engine events. The controller
 * replays from scratch on backward/seek (events are small, this is fast).
 */
export function createReplayController(events: EngineEvent[]): ReplayController {
  let currentIndex = 0;
  let currentSteps: ExecutionStep[] = [];
  let currentActiveLine: number | null = null;

  function replayTo(targetIndex: number): ReplayState {
    const clamped = Math.max(0, Math.min(targetIndex, events.length));
    let steps: ExecutionStep[] = [];
    const activeLineRef: ActiveLineRef = { current: null };
    const activeNodeRef: ActiveNodeRef = { current: null };

    for (let i = 0; i < clamped; i++) {
      // `i < clamped <= events.length` so the element is always present;
      // the cast satisfies `noUncheckedIndexedAccess` without a runtime guard.
      const event = events[i] as EngineEvent;
      const hubEvt: HubEvent = {
        type: 'Execution',
        payload: { project_id: 0, script_name: '', execution_id: '', event },
      };
      const result = reduceExecutionEvent(steps, hubEvt, activeLineRef, activeNodeRef);
      steps = result.steps;
    }

    currentIndex = clamped;
    currentSteps = steps;
    currentActiveLine = activeLineRef.current;

    return {
      steps,
      activeLine: activeLineRef.current,
      eventIndex: clamped,
      done: clamped >= events.length,
    };
  }

  const controller: ReplayController = {
    get eventIndex() { return currentIndex; },
    get totalEvents() { return events.length; },
    get steps() { return currentSteps; },
    get activeLine() { return currentActiveLine; },
    stepForward() { return replayTo(currentIndex + 1); },
    stepBackward() { return replayTo(currentIndex - 1); },
    seekTo(index: number) { return replayTo(index); },
  };

  return controller;
}
