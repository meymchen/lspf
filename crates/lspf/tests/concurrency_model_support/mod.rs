use std::collections::{HashMap, HashSet, VecDeque};

type RequestId = u8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct MessageId(u8);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TaskId(u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResponseTag(u8);

#[derive(Clone, Copy, Debug)]
struct ModeledResponse {
    request_id: RequestId,
    tag: ResponseTag,
}

impl ModeledResponse {
    fn new(request_id: RequestId, tag: u8) -> Self {
        Self {
            request_id,
            tag: ResponseTag(tag),
        }
    }
}

#[derive(Clone, Copy)]
struct OutboundExpectation {
    request_id: RequestId,
    tag: ResponseTag,
}

impl OutboundExpectation {
    fn new(request_id: RequestId, tag: u8) -> Self {
        Self {
            request_id,
            tag: ResponseTag(tag),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Completion {
    Cancelled,
    Success,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseCause {
    Eof,
    WriterFailed,
}

#[derive(Clone, Debug)]
enum Step {
    BeginOutbound(RequestId),
    Cancel(RequestId),
    Close,
    Complete(RequestId, Completion),
    Enqueue { entry: QueueEntry, required: bool },
    Quiesce,
    RequestClose(CloseCause),
    ReserveInbound(RequestId),
    ResolveOutbound(ModeledResponse),
    Send,
    SpawnTask(TaskId),
    WriterFail,
}

#[derive(Clone, Debug)]
struct Actor {
    name: &'static str,
    steps: Vec<Step>,
}

pub(crate) struct Scenario {
    name: &'static str,
    admitted: Vec<RequestId>,
    outbound: Vec<OutboundExpectation>,
    actors: Vec<Actor>,
}

impl Scenario {
    pub(crate) fn response_versus_cancellation() -> Self {
        Self {
            name: "response-versus-cancellation",
            admitted: vec![1, 2],
            outbound: Vec::new(),
            actors: vec![
                Actor {
                    name: "peer",
                    steps: vec![
                        Step::Complete(2, Completion::Success),
                        Step::Complete(1, Completion::Success),
                    ],
                },
                Actor {
                    name: "canceller",
                    steps: vec![Step::Cancel(1)],
                },
            ],
        }
    }

    pub(crate) fn out_of_order_outbound_responses() -> Self {
        Self {
            name: "out-of-order-outbound-responses",
            admitted: Vec::new(),
            outbound: vec![
                OutboundExpectation::new(10, 100),
                OutboundExpectation::new(11, 200),
            ],
            actors: vec![
                Actor {
                    name: "response-10",
                    steps: vec![Step::ResolveOutbound(ModeledResponse::new(10, 100))],
                },
                Actor {
                    name: "response-11",
                    steps: vec![Step::ResolveOutbound(ModeledResponse::new(11, 200))],
                },
            ],
        }
    }

    pub(crate) fn bounded_queue_versus_writer_and_close() -> Self {
        Self {
            name: "bounded-queue-versus-writer-and-close",
            admitted: Vec::new(),
            outbound: Vec::new(),
            actors: vec![
                Actor {
                    name: "producer-a",
                    steps: vec![Step::Enqueue {
                        entry: QueueEntry::new(10, 4),
                        required: false,
                    }],
                },
                Actor {
                    name: "producer-b",
                    steps: vec![Step::Enqueue {
                        entry: QueueEntry::new(11, 5),
                        required: false,
                    }],
                },
                Actor {
                    name: "writer",
                    steps: vec![Step::Send, Step::Send],
                },
                Actor {
                    name: "closer",
                    steps: vec![Step::Close],
                },
            ],
        }
    }

    pub(crate) fn task_and_request_versus_repeated_close() -> Self {
        Self {
            name: "task-and-request-versus-repeated-close",
            admitted: Vec::new(),
            outbound: Vec::new(),
            actors: vec![
                Actor {
                    name: "request",
                    steps: vec![
                        Step::BeginOutbound(20),
                        Step::ResolveOutbound(ModeledResponse::new(20, 20)),
                    ],
                },
                Actor {
                    name: "handler",
                    steps: vec![Step::SpawnTask(TaskId(7))],
                },
                Actor {
                    name: "eof-close",
                    steps: vec![Step::Close],
                },
                Actor {
                    name: "explicit-close",
                    steps: vec![Step::Close],
                },
            ],
        }
    }

    pub(crate) fn writer_failure_versus_eof() -> Self {
        Self {
            name: "writer-failure-versus-eof",
            admitted: Vec::new(),
            outbound: Vec::new(),
            actors: vec![
                Actor {
                    name: "ordinary-producer",
                    steps: vec![Step::Enqueue {
                        entry: QueueEntry::new(30, 8),
                        required: false,
                    }],
                },
                Actor {
                    name: "required-producer",
                    steps: vec![Step::Enqueue {
                        entry: QueueEntry::new(31, 1),
                        required: true,
                    }],
                },
                Actor {
                    name: "writer",
                    steps: vec![Step::WriterFail, Step::Quiesce],
                },
                Actor {
                    name: "reader",
                    steps: vec![Step::RequestClose(CloseCause::Eof), Step::Quiesce],
                },
            ],
        }
    }

    pub(crate) fn capacity_reuse_after_completion() -> Self {
        Self {
            name: "capacity-reuse-after-completion",
            admitted: vec![1, 2],
            outbound: Vec::new(),
            actors: vec![
                Actor {
                    name: "canceller",
                    steps: vec![
                        Step::Cancel(1),
                        Step::ReserveInbound(3),
                        Step::Complete(3, Completion::Success),
                    ],
                },
                Actor {
                    name: "peer",
                    steps: vec![
                        Step::Complete(2, Completion::Success),
                        Step::Complete(1, Completion::Success),
                    ],
                },
            ],
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct QueueEntry {
    message_id: MessageId,
    bytes: usize,
}

impl QueueEntry {
    fn new(message_id: u8, bytes: usize) -> Self {
        Self {
            message_id: MessageId(message_id),
            bytes,
        }
    }
}

#[derive(Clone, Default)]
struct Model {
    admitted: HashSet<RequestId>,
    in_flight: HashMap<RequestId, ()>,
    completions: HashMap<RequestId, Vec<Completion>>,
    queue: VecDeque<QueueEntry>,
    queued_bytes: usize,
    accepted: usize,
    released: usize,
    outbound_pending: HashSet<RequestId>,
    outbound_started: HashSet<RequestId>,
    outbound_expected: HashMap<RequestId, ResponseTag>,
    outbound_results: HashMap<RequestId, ResponseTag>,
    outbound_completions: HashMap<RequestId, Vec<Completion>>,
    tasks: HashSet<TaskId>,
    tasks_started: HashSet<TaskId>,
    tasks_joined: HashSet<TaskId>,
    cleanup_runs: usize,
    reservation_failures: Vec<String>,
    close_cause: Option<CloseCause>,
    writer_failure_before_quiescence: bool,
    closing: bool,
    closed: bool,
}

impl Model {
    fn admit(&mut self, id: RequestId) {
        assert!(self.admitted.insert(id));
        assert!(self.in_flight.insert(id, ()).is_none());
    }

    fn reserve_inbound(&mut self, id: RequestId) {
        if self.closing {
            self.reservation_failures
                .push(format!("request {id} was rejected after close"));
        } else if self.in_flight.len() == 2 {
            self.reservation_failures
                .push(format!("request {id} found no released capacity"));
        } else if self.admitted.contains(&id) {
            self.reservation_failures
                .push(format!("request {id} reused an admitted ID"));
        } else {
            self.admit(id);
        }
    }

    fn apply(&mut self, step: &Step) {
        match *step {
            Step::BeginOutbound(id) => self.begin_outbound(id),
            Step::Cancel(id) => self.complete(id, Completion::Cancelled),
            Step::Close => self.close(),
            Step::Complete(id, completion) => self.complete(id, completion),
            Step::Enqueue { entry, required } => self.enqueue(entry, required),
            Step::Quiesce => self.quiesce(),
            Step::RequestClose(cause) => self.request_close(cause),
            Step::ReserveInbound(id) => self.reserve_inbound(id),
            Step::ResolveOutbound(response) => self.resolve_outbound(response),
            Step::Send => self.release_front(),
            Step::SpawnTask(id) => self.spawn_task(id),
            Step::WriterFail => self.writer_fail(),
        }
    }

    fn complete(&mut self, id: RequestId, completion: Completion) {
        if self.in_flight.remove(&id).is_some() {
            self.completions.entry(id).or_default().push(completion);
        }
    }

    fn enqueue(&mut self, entry: QueueEntry, required: bool) {
        const MAX_MESSAGES: usize = 2;
        const MAX_BYTES: usize = 8;

        if self.closed {
            return;
        }
        if self.closing
            || self.queue.len() == MAX_MESSAGES
            || self.queued_bytes + entry.bytes > MAX_BYTES
        {
            if required {
                self.writer_failure_before_quiescence = true;
                self.request_close(CloseCause::WriterFailed);
            }
            return;
        }
        self.queue.push_back(entry);
        self.queued_bytes += entry.bytes;
        self.accepted += 1;
    }

    fn release_front(&mut self) {
        if let Some(entry) = self.queue.pop_front() {
            self.queued_bytes -= entry.bytes;
            self.released += 1;
        }
    }

    fn begin_outbound(&mut self, id: RequestId) {
        self.begin_outbound_expecting(id, ResponseTag(id));
    }

    fn begin_outbound_expecting(&mut self, id: RequestId, expected: ResponseTag) {
        if self.closing {
            return;
        }
        assert!(self.outbound_pending.insert(id));
        assert!(self.outbound_started.insert(id));
        assert!(self.outbound_expected.insert(id, expected).is_none());
    }

    fn resolve_outbound(&mut self, response: ModeledResponse) {
        if self.outbound_pending.remove(&response.request_id) {
            self.outbound_completions
                .entry(response.request_id)
                .or_default()
                .push(Completion::Success);
            self.outbound_results
                .insert(response.request_id, response.tag);
        }
    }

    fn spawn_task(&mut self, id: TaskId) {
        if self.closing {
            return;
        }
        assert!(self.tasks.insert(id));
        assert!(self.tasks_started.insert(id));
    }

    fn close(&mut self) {
        self.request_close(CloseCause::Eof);
        self.quiesce();
    }

    fn request_close(&mut self, cause: CloseCause) {
        if self.closed {
            return;
        }
        self.closing = true;
        if cause == CloseCause::WriterFailed || self.close_cause.is_none() {
            self.close_cause = Some(cause);
        }
    }

    fn writer_fail(&mut self) {
        if self.closed {
            return;
        }
        self.writer_failure_before_quiescence = true;
        self.request_close(CloseCause::WriterFailed);
        self.drain_queue();
    }

    fn quiesce(&mut self) {
        if self.closed || !self.closing {
            return;
        }
        self.drain_queue();
        for id in self.outbound_pending.drain() {
            self.outbound_completions
                .entry(id)
                .or_default()
                .push(Completion::Cancelled);
        }
        self.tasks_joined.extend(self.tasks.drain());
        self.cleanup_runs += 1;
        self.closed = true;
    }

    fn drain_queue(&mut self) {
        while !self.queue.is_empty() {
            self.release_front();
        }
    }

    fn check(&self) -> Result<(), String> {
        for id in &self.admitted {
            let count = self.completions.get(id).map_or(0, Vec::len);
            if count > 1 {
                return Err(format!("request {id} completed {count} times"));
            }
        }
        if self.in_flight.len() > 2 {
            return Err(format!(
                "{} inbound requests hold a two-request budget",
                self.in_flight.len()
            ));
        }
        if let Some(error) = self.reservation_failures.first() {
            return Err(error.clone());
        }
        if self.queue.len() > 2 {
            return Err(format!("queue contains {} messages", self.queue.len()));
        }
        let actual_bytes: usize = self.queue.iter().map(|entry| entry.bytes).sum();
        if actual_bytes != self.queued_bytes {
            return Err(format!(
                "queue charge is {} bytes but entries hold {actual_bytes}",
                self.queued_bytes
            ));
        }
        if self.queued_bytes > 8 {
            return Err(format!("queue is charged {} bytes", self.queued_bytes));
        }
        if self.accepted != self.released + self.queue.len() {
            return Err(format!(
                "{} messages accepted but {} released and {} remain",
                self.accepted,
                self.released,
                self.queue.len()
            ));
        }
        for id in &self.outbound_started {
            let count = self.outbound_completions.get(id).map_or(0, Vec::len);
            if count > 1 {
                return Err(format!("outbound request {id} completed {count} times"));
            }
            if let Some(actual) = self.outbound_results.get(id) {
                let expected = self
                    .outbound_expected
                    .get(id)
                    .expect("every started request records its expectation");
                if actual != expected {
                    return Err(format!(
                        "outbound request {id} received {actual:?}, expected {expected:?}"
                    ));
                }
            }
        }
        if self.cleanup_runs > 1 {
            return Err(format!("close cleanup ran {} times", self.cleanup_runs));
        }
        if !self.tasks_joined.is_subset(&self.tasks_started) {
            return Err("task group joined a task it never owned".to_string());
        }
        if self.writer_failure_before_quiescence
            && !self.closed
            && self.close_cause != Some(CloseCause::WriterFailed)
        {
            return Err(format!(
                "writer failure did not override close cause {:?}",
                self.close_cause
            ));
        }
        Ok(())
    }

    fn check_terminal(&self) -> Result<(), String> {
        self.check()?;
        for id in &self.admitted {
            let count = self.completions.get(id).map_or(0, Vec::len);
            if count != 1 {
                return Err(format!("request {id} completed {count} times"));
            }
        }
        if self.closed && !self.queue.is_empty() {
            let message_ids: Vec<_> = self.queue.iter().map(|entry| entry.message_id).collect();
            return Err(format!("closed queue retains messages {message_ids:?}"));
        }
        if self.closed && self.accepted != self.released {
            return Err(format!(
                "closed queue accepted {} messages but released {}",
                self.accepted, self.released
            ));
        }
        if self.closed {
            if self.cleanup_runs != 1 {
                return Err(format!("close cleanup ran {} times", self.cleanup_runs));
            }
            if !self.outbound_pending.is_empty() {
                return Err(format!(
                    "closed session retains pending requests {:?}",
                    self.outbound_pending
                ));
            }
            for id in &self.outbound_started {
                let count = self.outbound_completions.get(id).map_or(0, Vec::len);
                if count != 1 {
                    return Err(format!("outbound request {id} completed {count} times"));
                }
            }
            if self.tasks_started != self.tasks_joined {
                return Err(format!(
                    "task group started {:?} but joined {:?}",
                    self.tasks_started, self.tasks_joined
                ));
            }
            if self.close_cause.is_none() {
                return Err("closed session has no close cause".to_string());
            }
            if self.writer_failure_before_quiescence
                && self.close_cause != Some(CloseCause::WriterFailed)
            {
                return Err(format!(
                    "writer failure did not win final close cause {:?}",
                    self.close_cause
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn explore(scenario: Scenario) {
    if let Err(failure) = explore_scenario(&scenario) {
        panic!("{failure}");
    }
}

pub(crate) fn diagnostic_failure() -> String {
    let scenario = Scenario {
        name: "diagnostic-missing-completion",
        admitted: vec![99],
        outbound: Vec::new(),
        actors: vec![Actor {
            name: "writer",
            steps: vec![Step::Send],
        }],
    };
    explore_scenario(&scenario).expect_err("the diagnostic scenario omits completion")
}

fn explore_scenario(scenario: &Scenario) -> Result<(), String> {
    let mut model = Model::default();
    for id in &scenario.admitted {
        model.admit(*id);
    }
    for expectation in &scenario.outbound {
        model.begin_outbound_expecting(expectation.request_id, expectation.tag);
    }
    let mut cursors = vec![0; scenario.actors.len()];
    let mut trace = Vec::new();
    explore_next(scenario, model, &mut cursors, &mut trace)
}

fn explore_next(
    scenario: &Scenario,
    model: Model,
    cursors: &mut [usize],
    trace: &mut Vec<String>,
) -> Result<(), String> {
    let mut advanced = false;
    for actor_index in 0..scenario.actors.len() {
        let actor = &scenario.actors[actor_index];
        let cursor = cursors[actor_index];
        let Some(step) = actor.steps.get(cursor) else {
            continue;
        };
        advanced = true;

        let mut next = model.clone();
        next.apply(step);
        trace.push(format!("{}:{step:?}", actor.name));
        check_or_trace(scenario, trace, next.check())?;

        cursors[actor_index] += 1;
        explore_next(scenario, next, cursors, trace)?;
        cursors[actor_index] -= 1;
        trace.pop();
    }

    if !advanced {
        check_or_trace(scenario, trace, model.check_terminal())?;
    }
    Ok(())
}

fn check_or_trace(
    scenario: &Scenario,
    trace: &[String],
    result: Result<(), String>,
) -> Result<(), String> {
    result.map_err(|error| {
        format!(
            "concurrency model `{}` failed: {error}\nreplay trace:\n{}",
            scenario.name,
            trace.join("\n")
        )
    })
}
