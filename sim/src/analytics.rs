use crate::{AgentID, CarID, Event, TripID, TripMode, VehicleType};
use abstutil::Counter;
use derivative::Derivative;
use geom::{Distance, Duration, DurationHistogram, PercentageHistogram, Time};
use map_model::{
    BusRouteID, BusStopID, IntersectionID, Map, Path, PathRequest, RoadID, Traversable, TurnGroupID,
};
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Serialize, Deserialize, Derivative)]
pub struct Analytics {
    pub thruput_stats: ThruputStats,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) test_expectations: VecDeque<Event>,
    pub bus_arrivals: Vec<(Time, CarID, BusRouteID, BusStopID)>,
    pub bus_passengers_waiting: Vec<(Time, BusStopID, BusRouteID)>,
    // TODO Hack: No TripMode means aborted
    // Finish time, ID, mode (or None as aborted), trip duration
    pub finished_trips: Vec<(Time, TripID, Option<TripMode>, Duration)>,
    // TODO This subsumes finished_trips
    pub trip_log: Vec<(Time, TripID, Option<PathRequest>, String)>,
    pub intersection_delays: BTreeMap<IntersectionID, Vec<(Time, Duration)>>,

    // After we restore from a savestate, don't record anything. This is only going to make sense
    // if savestates are only used for quickly previewing against prebaked results, where we have
    // the full Analytics anyway.
    record_anything: bool,
}

#[derive(Clone, Serialize, Deserialize, Derivative)]
pub struct ThruputStats {
    #[serde(skip_serializing, skip_deserializing)]
    pub count_per_road: Counter<RoadID>,
    #[serde(skip_serializing, skip_deserializing)]
    pub count_per_intersection: Counter<IntersectionID>,

    raw_per_road: Vec<(Time, TripMode, RoadID)>,
    raw_per_intersection: Vec<(Time, TripMode, IntersectionID)>,

    // Unlike everything else in Analytics, this is just for a moment in time.
    pub demand: BTreeMap<TurnGroupID, usize>,
}

impl Analytics {
    pub fn new() -> Analytics {
        Analytics {
            thruput_stats: ThruputStats {
                count_per_road: Counter::new(),
                count_per_intersection: Counter::new(),
                raw_per_road: Vec::new(),
                raw_per_intersection: Vec::new(),
                demand: BTreeMap::new(),
            },
            test_expectations: VecDeque::new(),
            bus_arrivals: Vec::new(),
            bus_passengers_waiting: Vec::new(),
            finished_trips: Vec::new(),
            trip_log: Vec::new(),
            intersection_delays: BTreeMap::new(),
            record_anything: true,
        }
    }

    pub fn event(&mut self, ev: Event, time: Time, map: &Map) {
        if !self.record_anything {
            return;
        }

        // TODO Plumb a flag
        let raw_thruput = true;

        // Throughput
        if let Event::AgentEntersTraversable(a, to) = ev {
            let mode = match a {
                AgentID::Pedestrian(_) => TripMode::Walk,
                AgentID::Car(c) => match c.1 {
                    VehicleType::Car => TripMode::Drive,
                    VehicleType::Bike => TripMode::Bike,
                    VehicleType::Bus => TripMode::Transit,
                },
            };

            match to {
                Traversable::Lane(l) => {
                    let r = map.get_l(l).parent;
                    self.thruput_stats.count_per_road.inc(r);
                    if raw_thruput {
                        self.thruput_stats.raw_per_road.push((time, mode, r));
                    }
                }
                Traversable::Turn(t) => {
                    self.thruput_stats.count_per_intersection.inc(t.parent);
                    if raw_thruput {
                        self.thruput_stats
                            .raw_per_intersection
                            .push((time, mode, t.parent));
                    }

                    if let Some(id) = map.get_turn_group(t) {
                        *self.thruput_stats.demand.entry(id).or_insert(0) -= 1;
                    }
                }
            };
        }

        // Test expectations
        if !self.test_expectations.is_empty() && &ev == self.test_expectations.front().unwrap() {
            println!("At {}, met expectation {:?}", time, ev);
            self.test_expectations.pop_front();
        }

        // Bus arrivals
        if let Event::BusArrivedAtStop(bus, route, stop) = ev {
            self.bus_arrivals.push((time, bus, route, stop));
        }

        // Bus passengers
        if let Event::PedReachedBusStop(_, stop, route) = ev {
            self.bus_passengers_waiting.push((time, stop, route));
        }

        // Finished trips
        if let Event::TripFinished(id, mode, dt) = ev {
            self.finished_trips.push((time, id, Some(mode), dt));
        } else if let Event::TripAborted(id) = ev {
            self.finished_trips.push((time, id, None, Duration::ZERO));
        }

        // Intersection delays
        if let Event::IntersectionDelayMeasured(id, delay) = ev {
            self.intersection_delays
                .entry(id)
                .or_insert_with(Vec::new)
                .push((time, delay));
        }

        // TODO Kinda hacky, but these all consume the event, so kinda bundle em.
        match ev {
            Event::TripPhaseStarting(id, maybe_req, metadata) => {
                self.trip_log.push((time, id, maybe_req, metadata));
            }
            Event::TripAborted(id) => {
                self.trip_log
                    .push((time, id, None, format!("trip aborted for some reason")));
            }
            Event::TripFinished(id, _, _) => {
                self.trip_log
                    .push((time, id, None, format!("trip finished")));
            }
            Event::PathAmended(path) => {
                self.record_demand(&path, map);
            }
            _ => {}
        }
    }

    pub fn record_demand(&mut self, path: &Path, map: &Map) {
        for step in path.get_steps() {
            if let Traversable::Turn(t) = step.as_traversable() {
                if let Some(id) = map.get_turn_group(t) {
                    *self.thruput_stats.demand.entry(id).or_insert(0) += 1;
                }
            }
        }
    }

    // TODO If these ever need to be speeded up, just cache the histogram and index in the events
    // list.

    pub fn finished_trips(&self, now: Time, mode: TripMode) -> DurationHistogram {
        let mut distrib = DurationHistogram::new();
        for (t, _, m, dt) in &self.finished_trips {
            if *t > now {
                break;
            }
            if *m == Some(mode) {
                distrib.add(*dt);
            }
        }
        distrib
    }

    // Returns (all trips except aborted, number of aborted trips, trips by mode)
    pub fn all_finished_trips(
        &self,
        now: Time,
    ) -> (
        DurationHistogram,
        usize,
        BTreeMap<TripMode, DurationHistogram>,
    ) {
        let mut per_mode = TripMode::all()
            .into_iter()
            .map(|m| (m, DurationHistogram::new()))
            .collect::<BTreeMap<_, _>>();
        let mut all = DurationHistogram::new();
        let mut num_aborted = 0;
        for (t, _, m, dt) in &self.finished_trips {
            if *t > now {
                break;
            }
            if let Some(mode) = *m {
                all.add(*dt);
                per_mode.get_mut(&mode).unwrap().add(*dt);
            } else {
                num_aborted += 1;
            }
        }
        (all, num_aborted, per_mode)
    }

    // Returns unsorted list of deltas, one for each trip finished in both worlds. Positive dt
    // means faster.
    pub fn finished_trip_deltas(&self, now: Time, baseline: &Analytics) -> Vec<Duration> {
        let a: BTreeMap<TripID, Duration> = self
            .finished_trips
            .iter()
            .filter_map(|(t, id, mode, dt)| {
                if *t <= now && mode.is_some() {
                    Some((*id, *dt))
                } else {
                    None
                }
            })
            .collect();
        let b: BTreeMap<TripID, Duration> = baseline
            .finished_trips
            .iter()
            .filter_map(|(t, id, mode, dt)| {
                if *t <= now && mode.is_some() {
                    Some((*id, *dt))
                } else {
                    None
                }
            })
            .collect();

        a.into_iter()
            .filter_map(|(id, dt1)| b.get(&id).map(|dt2| *dt2 - dt1))
            .collect()
    }

    pub fn bus_arrivals(&self, now: Time, r: BusRouteID) -> BTreeMap<BusStopID, DurationHistogram> {
        let mut per_bus: BTreeMap<CarID, Vec<(Time, BusStopID)>> = BTreeMap::new();
        for (t, car, route, stop) in &self.bus_arrivals {
            if *t > now {
                break;
            }
            if *route == r {
                per_bus
                    .entry(*car)
                    .or_insert_with(Vec::new)
                    .push((*t, *stop));
            }
        }
        let mut delay_to_stop: BTreeMap<BusStopID, DurationHistogram> = BTreeMap::new();
        for events in per_bus.values() {
            for pair in events.windows(2) {
                delay_to_stop
                    .entry(pair[1].1)
                    .or_insert_with(DurationHistogram::new)
                    .add(pair[1].0 - pair[0].0);
            }
        }
        delay_to_stop
    }

    // TODO Refactor!
    // For each stop, a list of (time, delay)
    pub fn bus_arrivals_over_time(
        &self,
        now: Time,
        r: BusRouteID,
    ) -> BTreeMap<BusStopID, Vec<(Time, Duration)>> {
        let mut per_bus: BTreeMap<CarID, Vec<(Time, BusStopID)>> = BTreeMap::new();
        for (t, car, route, stop) in &self.bus_arrivals {
            if *t > now {
                break;
            }
            if *route == r {
                per_bus
                    .entry(*car)
                    .or_insert_with(Vec::new)
                    .push((*t, *stop));
            }
        }
        let mut delays_to_stop: BTreeMap<BusStopID, Vec<(Time, Duration)>> = BTreeMap::new();
        for events in per_bus.values() {
            for pair in events.windows(2) {
                delays_to_stop
                    .entry(pair[1].1)
                    .or_insert_with(Vec::new)
                    .push((pair[1].0, pair[1].0 - pair[0].0));
            }
        }
        delays_to_stop
    }

    // At some moment in time, what's the distribution of passengers waiting for a route like?
    pub fn bus_passenger_delays(
        &self,
        now: Time,
        r: BusRouteID,
    ) -> BTreeMap<BusStopID, DurationHistogram> {
        let mut waiting_per_stop = BTreeMap::new();
        for (t, stop, route) in &self.bus_passengers_waiting {
            if *t > now {
                break;
            }
            if *route == r {
                waiting_per_stop
                    .entry(*stop)
                    .or_insert_with(Vec::new)
                    .push(*t);
            }
        }

        for (t, _, route, stop) in &self.bus_arrivals {
            if *t > now {
                break;
            }
            if *route == r {
                if let Some(ref mut times) = waiting_per_stop.get_mut(stop) {
                    times.retain(|time| *time > *t);
                }
            }
        }

        waiting_per_stop
            .into_iter()
            .filter_map(|(k, v)| {
                let mut delays = DurationHistogram::new();
                for t in v {
                    delays.add(now - t);
                }
                if delays.count() == 0 {
                    None
                } else {
                    Some((k, delays))
                }
            })
            .collect()
    }

    // Slightly misleading -- TripMode::Transit means buses, not pedestrians taking transit
    pub fn throughput_road(
        &self,
        now: Time,
        road: RoadID,
        window_size: Duration,
    ) -> BTreeMap<TripMode, Vec<(Time, usize)>> {
        self.throughput(now, road, window_size, &self.thruput_stats.raw_per_road)
    }

    pub fn throughput_intersection(
        &self,
        now: Time,
        intersection: IntersectionID,
        window_size: Duration,
    ) -> BTreeMap<TripMode, Vec<(Time, usize)>> {
        self.throughput(
            now,
            intersection,
            window_size,
            &self.thruput_stats.raw_per_intersection,
        )
    }

    fn throughput<X: PartialEq>(
        &self,
        now: Time,
        obj: X,
        window_size: Duration,
        data: &Vec<(Time, TripMode, X)>,
    ) -> BTreeMap<TripMode, Vec<(Time, usize)>> {
        let mut pts_per_mode: BTreeMap<TripMode, Vec<(Time, usize)>> = BTreeMap::new();
        let mut windows_per_mode: BTreeMap<TripMode, Window> = BTreeMap::new();
        for mode in TripMode::all() {
            pts_per_mode.insert(mode, vec![(Time::START_OF_DAY, 0)]);
            windows_per_mode.insert(mode, Window::new(window_size));
        }

        for (t, m, x) in data {
            if *x != obj {
                continue;
            }
            if *t > now {
                break;
            }

            let count = windows_per_mode.get_mut(m).unwrap().add(*t);
            pts_per_mode.get_mut(m).unwrap().push((*t, count));
        }

        for (m, pts) in pts_per_mode.iter_mut() {
            let mut window = windows_per_mode.remove(m).unwrap();

            // Add a drop-off after window_size (+ a little epsilon!)
            let t = (pts.last().unwrap().0 + window_size + Duration::seconds(0.1)).min(now);
            if pts.last().unwrap().0 != t {
                pts.push((t, window.count(t)));
            }

            if pts.last().unwrap().0 != now {
                pts.push((now, window.count(now)));
            }
        }

        pts_per_mode
    }

    pub fn get_trip_phases(&self, trip: TripID, map: &Map) -> Vec<TripPhase> {
        let mut phases: Vec<TripPhase> = Vec::new();
        for (t, id, maybe_req, md) in &self.trip_log {
            if *id != trip {
                continue;
            }
            if let Some(ref mut last) = phases.last_mut() {
                last.end_time = Some(*t);
            }
            if md == "trip finished" || md == "trip aborted for some reason" {
                break;
            }
            phases.push(TripPhase {
                start_time: *t,
                end_time: None,
                // Unwrap should be safe, because this is the request that was actually done...
                path: maybe_req
                    .as_ref()
                    .map(|req| (req.start.dist_along(), map.pathfind(req.clone()).unwrap())),
                description: md.clone(),
            })
        }
        phases
    }

    fn get_all_trip_phases(&self) -> BTreeMap<TripID, Vec<TripPhase>> {
        let mut trips = BTreeMap::new();
        for (t, id, _, md) in &self.trip_log {
            let phases: &mut Vec<TripPhase> = trips.entry(*id).or_insert_with(Vec::new);
            if let Some(ref mut last) = phases.last_mut() {
                last.end_time = Some(*t);
            }
            if md == "trip finished" {
                continue;
            }
            // Remove aborted trips
            if md == "trip aborted for some reason" {
                trips.remove(id);
                continue;
            }
            phases.push(TripPhase {
                start_time: *t,
                end_time: None,
                // Don't compute any paths
                path: None,
                description: md.clone(),
            })
        }
        trips
    }

    pub fn analyze_parking_phases(&self) -> Vec<String> {
        // Of all completed trips involving parking, what percentage of total time was spent as
        // "overhead" -- not the main driving part of the trip?
        // TODO This is misleading for border trips -- the driving lasts longer.
        let mut distrib = PercentageHistogram::new();
        for (_, phases) in self.get_all_trip_phases() {
            if phases.last().as_ref().unwrap().end_time.is_none() {
                continue;
            }
            let mut driving_time = Duration::ZERO;
            let mut overhead = Duration::ZERO;
            for p in phases {
                let dt = p.end_time.unwrap() - p.start_time;
                // TODO New enum instead of strings, if there'll be more analyses like this
                if p.description.starts_with("CarID(") {
                    driving_time += dt;
                } else if p.description == "parking somewhere else"
                    || p.description == "parking on the current lane"
                {
                    overhead += dt;
                } else if p.description.starts_with("PedestrianID(") {
                    overhead += dt;
                } else {
                    // Waiting for a bus. Irrelevant.
                }
            }
            // Only interested in trips with both
            if driving_time == Duration::ZERO || overhead == Duration::ZERO {
                continue;
            }
            distrib.add(overhead / (driving_time + overhead));
        }
        vec![
            format!("Consider all trips with both a walking and driving portion"),
            format!(
                "The portion of the trip spent walking to the parked car, looking for parking, \
                 and walking from the parking space to the final destination are all overhead."
            ),
            format!(
                "So what's the distribution of overhead percentages look like? 0% is ideal -- the \
                 entire trip is spent just driving between the original source and destination."
            ),
            distrib.describe(),
        ]
    }

    pub fn intersection_delays(&self, i: IntersectionID, t1: Time, t2: Time) -> DurationHistogram {
        let mut delays = DurationHistogram::new();
        // TODO Binary search
        if let Some(list) = self.intersection_delays.get(&i) {
            for (t, dt) in list {
                if *t < t1 {
                    continue;
                }
                if *t > t2 {
                    break;
                }
                delays.add(*dt);
            }
        }
        delays
    }

    pub fn intersection_delays_bucketized(
        &self,
        now: Time,
        i: IntersectionID,
        bucket: Duration,
    ) -> Vec<(Time, DurationHistogram)> {
        let mut max_this_bucket = now.min(Time::START_OF_DAY + bucket);
        let mut results = vec![
            (Time::START_OF_DAY, DurationHistogram::new()),
            (max_this_bucket, DurationHistogram::new()),
        ];
        if let Some(list) = self.intersection_delays.get(&i) {
            for (t, dt) in list {
                if *t > now {
                    break;
                }
                if *t > max_this_bucket {
                    max_this_bucket = now.min(max_this_bucket + bucket);
                    results.push((max_this_bucket, DurationHistogram::new()));
                }
                results.last_mut().unwrap().1.add(*dt);
            }
        }
        results
    }
}

impl Default for Analytics {
    fn default() -> Analytics {
        let mut a = Analytics::new();
        a.record_anything = false;
        a
    }
}

pub struct TripPhase {
    pub start_time: Time,
    pub end_time: Option<Time>,
    // Plumb along start distance
    pub path: Option<(Distance, Path)>,
    pub description: String,
}

impl TripPhase {
    pub fn describe(&self, now: Time) -> String {
        if let Some(t2) = self.end_time {
            format!(
                "{} .. {} ({}): {}",
                self.start_time,
                t2,
                t2 - self.start_time,
                self.description
            )
        } else {
            format!(
                "{} .. ongoing ({} so far): {}",
                self.start_time,
                now - self.start_time,
                self.description
            )
        }
    }
}

struct Window {
    times: VecDeque<Time>,
    window_size: Duration,
}

impl Window {
    fn new(window_size: Duration) -> Window {
        Window {
            times: VecDeque::new(),
            window_size,
        }
    }

    // Returns the count at time
    fn add(&mut self, time: Time) -> usize {
        self.times.push_back(time);
        self.count(time)
    }

    // Grab the count at this time, but don't add a new time
    fn count(&mut self, end: Time) -> usize {
        while !self.times.is_empty() && end - *self.times.front().unwrap() > self.window_size {
            self.times.pop_front();
        }
        self.times.len()
    }
}
