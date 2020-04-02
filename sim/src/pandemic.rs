use crate::{CarID, Command, Event, Person, PersonID, Scheduler, TripPhaseType};
use geom::{Duration, Time};
use map_model::{BuildingID, BusStopID};
use rand::Rng;
use rand_xorshift::XorShiftRng;
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// TODO This does not model transmission by surfaces; only person-to-person.
// TODO If two people are in the same shared space indefinitely and neither leaves, we don't model
// transmission. It only occurs when people leave a space.

#[derive(Clone)]
pub struct PandemicModel {
    pub infected: BTreeSet<PersonID>,
    hospitalized: BTreeSet<PersonID>,

    bldgs: SharedSpace<BuildingID>,
    bus_stops: SharedSpace<BusStopID>,
    buses: SharedSpace<CarID>,
    person_to_bus: BTreeMap<PersonID, CarID>,

    rng: XorShiftRng,
    initialized: bool,
}

// You can schedule callbacks in the future by doing scheduler.push(future time, one of these)
#[derive(Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
pub enum Cmd {
    BecomeHospitalized(PersonID),
}

// TODO Pretend handle_event and handle_cmd also take in some object that lets you do things like:
//
// - replace_future_trips(PersonID, Vec<IndividTrip>)
//
// I'm not exactly sure how this should work yet. Any place you want to change the rest of the
// simulation, just add a comment describing what you want to do exactly, and we'll figure it out
// from there.

impl PandemicModel {
    pub fn new(rng: XorShiftRng) -> PandemicModel {
        PandemicModel {
            infected: BTreeSet::new(),
            hospitalized: BTreeSet::new(),

            bldgs: SharedSpace::new(),
            bus_stops: SharedSpace::new(),
            buses: SharedSpace::new(),
            person_to_bus: BTreeMap::new(),

            rng,
            initialized: false,
        }
    }

    // Sorry, initialization order of simulations is still a bit messy. This'll be called at
    // Time::START_OF_DAY after all of the people have been created from a Scenario.
    pub fn initialize(&mut self, population: &Vec<Person>, scheduler: &mut Scheduler) {
        assert!(!self.initialized);
        self.initialized = true;

        // Seed initially infected people.
        for p in population {
            if self.rng.gen_bool(0.1) {
                self.become_infected(Time::START_OF_DAY, p.id, scheduler);
            }
        }
    }

    pub fn handle_event(&mut self, now: Time, ev: &Event, scheduler: &mut Scheduler) {
        assert!(self.initialized);

        match ev {
            Event::PersonEntersBuilding(person, bldg) => {
                self.bldgs.person_enters_space(now, *person, *bldg);
            }
            Event::PersonLeavesBuilding(person, bldg) => {
                if let Some(others) = self.bldgs.person_leaves_space(now, *person, *bldg) {
                    self.transmission(now, *person, others, scheduler);
                } else {
                    // TODO A person left a building, but they weren't inside of it? Not sure
                    // what's happening here yet.
                }
            }
            Event::TripPhaseStarting(_, p, _, _, tpt) => {
                let person = *p;
                match tpt {
                    TripPhaseType::WaitingForBus(_, stop) => {
                        self.bus_stops.person_enters_space(now, person, *stop);
                    }
                    TripPhaseType::RidingBus(_, stop, bus) => {
                        let others = self
                            .bus_stops
                            .person_leaves_space(now, person, *stop)
                            .unwrap();
                        self.transmission(now, person, others, scheduler);

                        self.buses.person_enters_space(now, person, *bus);
                        self.person_to_bus.insert(person, *bus);
                    }
                    TripPhaseType::Walking => {
                        // A person can start walking for many reasons, but the only possible state
                        // transition after riding a bus is walking, so use this to detect the end
                        // of a bus ride.
                        if let Some(car) = self.person_to_bus.remove(&person) {
                            let others = self.buses.person_leaves_space(now, person, car).unwrap();
                            self.transmission(now, person, others, scheduler);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    pub fn handle_cmd(&mut self, _now: Time, cmd: Cmd, _scheduler: &mut Scheduler) {
        assert!(self.initialized);

        match cmd {
            Cmd::BecomeHospitalized(person) => {
                self.hospitalized.insert(person);
            }
        }
    }

    fn transmission(
        &mut self,
        now: Time,
        person: PersonID,
        other_occupants: Vec<(PersonID, Duration)>,
        scheduler: &mut Scheduler,
    ) {
        // person has spent some duration in the same space as other people. Does transmission
        // occur?
        for (other, overlap) in other_occupants {
            if self.infected.contains(&person) != self.infected.contains(&other) {
                if overlap > Duration::hours(1) && self.rng.gen_bool(0.1) {
                    if self.infected.contains(&person) {
                        self.become_infected(now, other, scheduler);
                    } else {
                        self.become_infected(now, person, scheduler);
                    }
                }
            }
        }
    }

    fn become_infected(&mut self, now: Time, person: PersonID, scheduler: &mut Scheduler) {
        self.infected.insert(person);

        if self.rng.gen_bool(0.1) {
            scheduler.push(
                now + self.rand_duration(Duration::hours(1), Duration::hours(3)),
                Command::Pandemic(Cmd::BecomeHospitalized(person)),
            );
        }
    }

    fn rand_duration(&mut self, low: Duration, high: Duration) -> Duration {
        assert!(high > low);
        Duration::seconds(
            self.rng
                .gen_range(low.inner_seconds(), high.inner_seconds()),
        )
    }
}

#[derive(Clone)]
struct SharedSpace<T: Ord> {
    // Since when has a person been in some shared space?
    // TODO This is an awkward data structure; abstutil::MultiMap is also bad, because key removal
    // would require knowing the time. Want something closer to
    // https://guava.dev/releases/19.0/api/docs/com/google/common/collect/Table.html.
    occupants: BTreeMap<T, Vec<(PersonID, Time)>>,
}

impl<T: Ord> SharedSpace<T> {
    fn new() -> SharedSpace<T> {
        SharedSpace {
            occupants: BTreeMap::new(),
        }
    }

    fn person_enters_space(&mut self, now: Time, person: PersonID, space: T) {
        self.occupants
            .entry(space)
            .or_insert_with(Vec::new)
            .push((person, now));
    }

    // Returns a list of all other people that the person was in the shared space with, and how
    // long their time overlapped. If it returns None, then a bug must have occurred, because
    // somebody has left a space they never entered.
    fn person_leaves_space(
        &mut self,
        now: Time,
        person: PersonID,
        space: T,
    ) -> Option<Vec<(PersonID, Duration)>> {
        // TODO Messy to mutate state inside a retain closure
        let mut inside_since: Option<Time> = None;
        let occupants = self.occupants.entry(space).or_insert_with(Vec::new);
        occupants.retain(|(p, t)| {
            if *p == person {
                inside_since = Some(*t);
                false
            } else {
                true
            }
        });
        // TODO Bug!
        let inside_since = inside_since?;

        Some(
            occupants
                .iter()
                .map(|(p, t)| (*p, now - (*t).max(inside_since)))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(x: usize) -> Time {
        Time::START_OF_DAY + Duration::hours(x)
    }

    #[test]
    fn test_overlap() {
        let mut space = SharedSpace::new();
        let mut now = time(0);

        let bldg1 = BuildingID(1);
        let bldg2 = BuildingID(2);

        let person1 = PersonID(1);
        let person2 = PersonID(2);
        let person3 = PersonID(3);

        // Only one person
        space.person_enters_space(now, person1, bldg1);
        now = time(1);
        assert_eq!(
            space.person_leaves_space(now, person1, bldg1),
            Some(Vec::new())
        );

        // Two people at the same time
        now = time(2);
        space.person_enters_space(now, person1, bldg2);
        space.person_enters_space(now, person2, bldg2);
        now = time(3);
        assert_eq!(
            space.person_leaves_space(now, person1, bldg2),
            Some(vec![(person2, Duration::hours(1))])
        );

        // Bug
        assert_eq!(space.person_leaves_space(now, person3, bldg2), None);

        // Different times
        now = time(5);
        space.person_enters_space(now, person1, bldg1);
        now = time(6);
        space.person_enters_space(now, person2, bldg1);
        now = time(7);
        space.person_enters_space(now, person3, bldg1);
        now = time(10);
        assert_eq!(
            space.person_leaves_space(now, person1, bldg1),
            Some(vec![
                (person2, Duration::hours(4)),
                (person3, Duration::hours(3))
            ])
        );
        now = time(12);
        assert_eq!(
            space.person_leaves_space(now, person2, bldg1),
            Some(vec![(person3, Duration::hours(5))])
        );
    }
}
