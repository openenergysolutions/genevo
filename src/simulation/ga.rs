//! This module provides the `Simulator` which implements the genetic algorithm
//! (GA) and the related `SimulatorBuilder`.
//!
//! The stages of the basic genetic algorithm are:
//!
//! 1. **Initialize**: Generate random population of n genotypes (or chromosomes)
//! 2. **Fitness**: Evaluate the fitness of each genotype in the population
//! 3. **New Population**: Create a new population by repeating following steps
//!    until the new population is complete:
//! 3.1. **Selection**: Select a tuple of parent genotypes from a population
//!      according to their fitness and the selection strategy of the
//!      configured `operator::SelectionOp`
//! 3.2. **Crossover**: With a crossover probability cross over the parents to
//!      form a new offspring (child) by means of the configured
//!      `operator::CrossoverOp`.
//! 3.3. **Mutation**: With a mutation probability mutate new offspring at each
//!      locus (position in genotype) by means of the configured
//!      `operator::MutationOp`.
//! 3.4. **Accepting**: Place new offspring in the new population.
//! 4. **Replace**: Use new generated population for a further run of the
//!    algorithm.
//! 5. **Termination**: If the end condition is satisfied, stop, and return the
//!    best solution in current population.
//! 6. **Loop**: Go to step 2
//!
//! The `Simulator` implements the `simulation::Simulation` trait. The
//! `SimulatorBuilder` implements the `simulation::SimulationBuilder` trait.

use algorithm::GeneticAlgorithm;
use chrono::{DateTime, Duration, Local};
use genetic::{Fitness, FitnessFunction, Genotype, Offspring, Parents};
use operator::{CrossoverOp, MutationOp, ReinsertionOp, SelectionOp};
use population::Population;
use random::{Prng, RngJump, Seed, get_rng, random_seed};
use simulation::{BestSolution, Evaluated, EvaluatedPopulation, SimError, SimResult, Simulation,
                 SimulationBuilder, State};
use statistic::{TimedResult, timed};
use termination::{StopFlag, Termination};
use rayon;
use std::marker::PhantomData;
use std::mem;
use std::rc::Rc;

//TODO make MIN_POPULATION_SIZE a parameter of the Simulator
const MIN_POPULATION_SIZE: usize = 7;

/// The `simulate` method implements the 'initialization' stage (step 1) of
/// the genetic algorithm.
pub fn simulate<G, F, E, S, C, M, R, Q>(algorithm: GeneticAlgorithm<G, F, E, S, C, M, R, Q>)
    -> SimulatorWithAlgorithmBuilder<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness, E: FitnessFunction<G, F>, S: SelectionOp<G, F>,
          C: CrossoverOp<G>, M: MutationOp<G>, R: ReinsertionOp<G, F>, Q: Termination<G, F>
{
    SimulatorWithAlgorithmBuilder {
        algorithm: algorithm
    }
}

pub struct SimulatorWithAlgorithmBuilder<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness, E: FitnessFunction<G, F>, S: SelectionOp<G, F>,
          C: CrossoverOp<G>, M: MutationOp<G>, R: ReinsertionOp<G, F>, Q: Termination<G, F>
{
    algorithm: GeneticAlgorithm<G, F, E, S, C, M, R, Q>,
}

impl<G, F, E, S, C, M, R, Q> SimulatorWithAlgorithmBuilder<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness, E: FitnessFunction<G, F>, S: SelectionOp<G, F>,
          C: CrossoverOp<G>, M: MutationOp<G>, R: ReinsertionOp<G, F>, Q: Termination<G, F>
{
    pub fn with_initial_population(self, initial_population: Population<G>)
        -> SimulatorBuilder<G, F, E, S, C, M, R, Q>
    {
        SimulatorBuilder {
            algorithm: self.algorithm,
            initial_population: initial_population,
        }
    }
}

pub struct SimulatorBuilder<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness, E: FitnessFunction<G, F>, S: SelectionOp<G, F>,
          C: CrossoverOp<G>, M: MutationOp<G>, R: ReinsertionOp<G, F>, Q: Termination<G, F>
{
    algorithm: GeneticAlgorithm<G, F, E, S, C, M, R, Q>,
    initial_population: Population<G>,
}

impl<G, F, E, S, C, M, R, Q> SimulatorBuilder<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness, E: FitnessFunction<G, F>, S: SelectionOp<G, F>,
          C: CrossoverOp<G>, M: MutationOp<G>, R: ReinsertionOp<G, F>, Q: Termination<G, F>
{
    fn build_simulator(self) -> Simulator<G, F, E, S, C, M, R, Q> {
        Simulator {
            _f: PhantomData,
            evaluator: self.algorithm.evaluator().clone(),
            selector: self.algorithm.selector().clone(),
            breeder: self.algorithm.breeder().clone(),
            mutator: self.algorithm.mutator().clone(),
            reinserter: self.algorithm.reinserter().clone(),
            termination: self.algorithm.termination().clone(),
            run_mode: RunMode::NotRunning,
            started_at: Local::now(),
            generation: 1,
            population: Rc::new(self.initial_population.individuals().to_vec()),
            processing_time: Duration::zero(),
            finished: false,
            initial_population: self.initial_population,
        }
    }
}

impl<G, F, E, S, C, M, R, Q> SimulationBuilder<Simulator<G, F, E, S, C, M, R, Q>, GeneticAlgorithm<G, F, E, S, C, M, R, Q>, G, F>
    for SimulatorBuilder<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness + Send + Sync, E: FitnessFunction<G, F> + Sync, S: SelectionOp<G, F>,
          C: CrossoverOp<G> + Sync, M: MutationOp<G> + Sync, R: ReinsertionOp<G, F>, Q: Termination<G, F>
{
    fn build(self) -> Simulator<G, F, E, S, C, M, R, Q> {
        self.build_simulator()
    }
}

/// The `RunMode` identifies whether the simulation is running and how it has
/// been started.
enum RunMode {
    /// The simulation is running in loop mode. i.e. it was started by calling
    /// the `run` function.
    Loop,
    /// The simulation is running in step mode. i.e. it was started by calling
    /// the `step` function.
    Step,
    /// The simulation is not running.
    NotRunning,
}

pub struct Simulator<G, F, E, S, C, M, R, Q>
    where G: Genotype, F: Fitness,
          E: FitnessFunction<G, F>, S: SelectionOp<G, F>, Q: Termination<G, F>,
          C: CrossoverOp<G>, M: MutationOp<G>, R: ReinsertionOp<G, F>
{
    evaluator: E,
    selector: S,
    breeder: C,
    mutator: M,
    reinserter: R,
    termination: Q,
    initial_population: Population<G>,
    run_mode: RunMode,
    started_at: DateTime<Local>,
    generation: u64,
    population: Rc<Vec<G>>,
    processing_time: Duration,
    finished: bool,
    _f: PhantomData<F>,
}

impl<G, F, E, S, C, M, R, Q> Simulation<GeneticAlgorithm<G, F, E, S, C, M, R, Q>, G, F>
    for Simulator<G, F, E, S, C, M, R, Q>
    where G: Genotype + Send + Sync, F: Fitness + Send + Sync,
          E: FitnessFunction<G, F> + Sync, S: SelectionOp<G, F>, Q: Termination<G, F>,
          C: CrossoverOp<G> + Sync, M: MutationOp<G> + Sync, R: ReinsertionOp<G, F>
{
    fn run(&mut self) -> Result<SimResult<G, F>, SimError> {
        match self.run_mode {
            RunMode::Loop =>
                return Err(SimError::SimulationAlreadyRunning(
                    format!("Simulation already running in loop since {}", &self.started_at))),
            RunMode::Step =>
                return Err(SimError::SimulationAlreadyRunning(
                    format!("Simulation already running in step mode since {}", &self.started_at))),
            RunMode::NotRunning => {
                self.run_mode = RunMode::Loop;
                self.started_at = Local::now();
            },
        }
        let mut result = Err(SimError::UnexpectedError("Unexpected error! \
                             No loop of the simulation has ever been processed!".to_string()));
        self.finished = false;
        while !self.finished {
            // Stages 2-4: Look at one generation
            result = self.process_one_generation(random_seed()).and_then(|state| {
                // Stage 5: Be aware of the termination:
                Ok(match self.termination.evaluate(&state) {
                    StopFlag::Continue => {
                        SimResult::Intermediate(state)
                    },
                    StopFlag::StopNow(reason) => {
                        self.finished = true;
                        let duration = Local::now().signed_duration_since(self.started_at);
                        SimResult::Final(state, duration, reason)
                    },
                })
            }).or_else(|error| {
                self.finished = true;
                Err(error)
            });
        }
        self.run_mode = RunMode::NotRunning;
        result
    }

    fn step(&mut self) -> Result<SimResult<G, F>, SimError> {
        self.step_with_seed(random_seed())
    }

    fn step_with_seed(&mut self, seed: Seed) -> Result<SimResult<G, F>, SimError> {
        match self.run_mode {
            RunMode::Loop =>
                return Err(SimError::SimulationAlreadyRunning(
                    format!("Simulation already running in loop since {}", &self.started_at))),
            RunMode::Step => (),
            RunMode::NotRunning => {
                    self.run_mode = RunMode::Step;
                    self.started_at = Local::now();
                },
        }
        if self.population.len() < MIN_POPULATION_SIZE {
            return Err(SimError::PopulationTooSmall(
                format!("Population of generation {} has a size of {} which is smaller than the \
                        required minimum size of {}",
                        self.generation, self.initial_population.size(), MIN_POPULATION_SIZE)))
        }

        // Stages 2-4: Look at one generation
        self.process_one_generation(seed).and_then(|state|

            // Stage 5: Be aware of the termination:
            Ok(match self.termination.evaluate(&state) {
                StopFlag::Continue => {
                    SimResult::Intermediate(state)
                },
                StopFlag::StopNow(reason) => {
                    let duration = Local::now().signed_duration_since(self.started_at);
                    self.run_mode = RunMode::NotRunning;
                    SimResult::Final(state, duration, reason)
                },
            })
        )
    }

    fn stop(&mut self) -> Result<bool, SimError> {
        match self.run_mode {
            RunMode::Loop | RunMode::Step => {
                self.finished = true;
                Ok(true)
            },
            RunMode::NotRunning =>
                Ok(false)
        }
    }

    fn reset(&mut self) -> Result<bool, SimError> {
        match self.run_mode {
            RunMode::Loop =>
                return Err(SimError::SimulationAlreadyRunning(
                    format!("Simulation still running in loop mode since {}. Wait for the \
                             simulation to finish or stop it before resetting it.",
                            &self.started_at))),
            RunMode::Step =>
                return Err(SimError::SimulationAlreadyRunning(
                    format!("Simulation still running in step mode since {}. Wait for the \
                             simulation to finish or stop it before resetting it.",
                             &self.started_at))),
            RunMode::NotRunning => (),
        }
        self.run_mode = RunMode::NotRunning;
        self.processing_time = Duration::zero();
        self.generation = 1;
        self.population = Rc::new(self.initial_population.individuals().to_vec());
        Ok(true)
    }
}

impl<G, F, E, S, C, M, R, Q> Simulator<G, F, E, S, C, M, R, Q>
    where G: Genotype + Send + Sync, F: Fitness + Send + Sync,
          E: FitnessFunction<G, F> + Sync, S: SelectionOp<G, F>, Q: Termination<G, F>,
          C: CrossoverOp<G> + Sync, M: MutationOp<G> + Sync, R: ReinsertionOp<G, F>
{
    /// Processes stages 2-4 of the genetic algorithm
    fn process_one_generation(&mut self, seed: Seed) -> Result<State<G, F>, SimError> {
        let loop_started_at = Local::now();

        // Stage 2: The fitness check:
//        let (score_board, eval_proc_time1) = self.evaluate_fitness(self.population.clone());
        let (score_board, eval_proc_time1) = evaluate_fitness(self.population.clone(), &self.evaluator);
        let (best_solution, eval_proc_time2) = self.determine_best_solution(&score_board);

        // Stage 3: The making of a new population:
        let (next_generation, new_pop_proc_time) = self.create_new_population(&score_board, get_rng(seed));
        next_generation.and_then(|next_generation| {

            // Stage 4: On to the next generation:
            let loop_processing_time = eval_proc_time1 + eval_proc_time2 + new_pop_proc_time;
            self.processing_time = self.processing_time + loop_processing_time;
            let loop_duration = Local::now().signed_duration_since(loop_started_at);
            Ok(self.replace_generation(loop_duration, loop_processing_time, seed,
                                       score_board, best_solution, next_generation))
        })
    }

    /// Determines the best solution of the current population
    fn determine_best_solution(&self, score_board: &EvaluatedPopulation<G, F>)
        -> (BestSolution<G, F>, Duration) {
        let started_at = Local::now();
        let index_of_best = score_board.index_of_fitness(&score_board.highest_fitness)
            .expect(&format!("No fitness value of {:?} found in this EvaluatedPopulation",
                             &score_board.highest_fitness));
        let evaluated = Evaluated {
            genome: self.population[index_of_best].clone(),
            fitness: score_board.fitness_values[index_of_best].clone(),
        };
        (BestSolution {
            found_at: Local::now(),
            generation: self.generation,
            solution: evaluated,
        },
        Local::now().signed_duration_since(started_at))
    }

    /// Creates a new population which is derived from the current population
    /// applying 'Selection', 'Crossover' and 'Mutation'.
    fn create_new_population(&self, evaluated_population: &EvaluatedPopulation<G, F>, rng: Prng)
        -> (Result<Vec<G>, SimError>, Duration) {
        let started_at = Local::now();
        let mut rng = rng;
        let new_population = self.selector.select_from(evaluated_population, &mut rng)
            .and_then(|selection|
//                self.breed_offspring(selection, &mut rng))
                par_breed_offspring(selection, &self.breeder, &self.mutator, rng))
            .and_then(|mut offspring|
                self.reinserter.combine(&mut offspring, evaluated_population, &mut rng));
        (new_population, Local::now().signed_duration_since(started_at))
    }

    /// Creates a `simulation::State` object about the last processed evolution,
    /// replaces the current generation with the next generation and increases
    /// the generation counter.
    fn replace_generation(&mut self,
                          loop_time: Duration,
                          processing_time: Duration,
                          used_seed: Seed,
                          score_board: EvaluatedPopulation<G, F>,
                          best_solution: BestSolution<G, F>,
                          next_population: Vec<G>,
                         ) -> State<G, F> {
        let curr_generation = self.generation;
        let curr_population = mem::replace(&mut self.population, Rc::new(next_population));
//        let curr_p = Rc::try_unwrap(curr_p).expect("Can not unwrap Rc(Vec<G>)");
        self.generation += 1;
        State {
            started_at: self.started_at,
            generation: curr_generation,
            seed: used_seed,
            population: curr_population.to_vec(),
            fitness_values: score_board.fitness_values,
            duration: loop_time,
            processing_time: processing_time,
            average_fitness: score_board.average_fitness,
            highest_fitness: score_board.highest_fitness,
            lowest_fitness: score_board.lowest_fitness,
            best_solution: best_solution,
        }
    }
}

fn evaluate_fitness<G, F, E>(population: Rc<Vec<G>>, evaluator: &E)
    -> (EvaluatedPopulation<G, F>, Duration)
    where G: Genotype + Sync, F: Fitness + Send + Sync, E: FitnessFunction<G, F> + Sync {
    let timed = par_evaluate_fitness(&population, evaluator);
    let average = evaluator.average(&timed.result.0);
    (EvaluatedPopulation {
        individuals: population,
        fitness_values: timed.result.0,
        highest_fitness: timed.result.1,
        lowest_fitness: timed.result.2,
        average_fitness: average,
    }, timed.time.duration())
}

/// Calculates the `genetic::Fitness` value of each `genetic::Genotype` and
/// records the highest and lowest values.
fn par_evaluate_fitness<G, F, E>(population: &[G], evaluator: &E)
    -> TimedResult<(Vec<F>, F, F)>
    where G: Genotype + Sync, F: Fitness + Send + Sync, E: FitnessFunction<G, F> + Sync {
    if population.len() < 60 {
        timed(|| {
            let mut fitness = Vec::with_capacity(population.len());
            let mut highest = evaluator.lowest_possible_fitness();
            let mut lowest = evaluator.highest_possible_fitness();
            for genome in population.iter() {
                let score = evaluator.fitness_of(genome);
                if score > highest {
                    highest = score.clone();
                }
                if score < lowest {
                    lowest = score.clone();
                }
                fitness.push(score);
            }
            (fitness, highest, lowest)
        }).run()
    } else {
        let mid_point = population.len() / 2;
        let (l_slice, r_slice) = population.split_at(mid_point);
        let (mut left, mut right) = rayon::join(|| par_evaluate_fitness(l_slice, evaluator),
                                                || par_evaluate_fitness(r_slice, evaluator));
        let mut fitness = Vec::with_capacity(population.len());
        fitness.append(&mut left.result.0);
        fitness.append(&mut right.result.0);
        let highest = if left.result.1 >= right.result.1 {
            left.result.1
        } else {
            right.result.1
        };
        let lowest = if left.result.2 <= right.result.2 {
            left.result.2
        } else {
            right.result.2
        };
        TimedResult {
            result: (fitness, highest, lowest),
            time: left.time + right.time,
        }
    }
}

/// Lets the parents breed their offspring and mutate its children. And
/// finally combines the offspring of all parents into one big offspring.
fn par_breed_offspring<G, C, M>(parents: Vec<Parents<G>>, breeder: &C, mutator: &M, rng: Prng)
    -> Result<Offspring<G>, SimError>
    where G: Genotype + Send, C: CrossoverOp<G> + Sync, M: MutationOp<G> + Sync
{
    if parents.len() < 60 {
        let mut rng = rng;
        let mut offspring: Offspring<G> = Vec::with_capacity(parents.len() * parents[0].len());
        for parents in parents {
            match breeder.crossover(parents, &mut rng) {
                Ok(children) => {
                    for child in children {
                        match mutator.mutate(child, &mut rng) {
                            Ok(mutated) => {
                                offspring.push(mutated);
                            },
                            Err(error) =>
                                return Err(error),
                        }
                    }
                },
                Err(error) =>
                    return Err(error),
            }
        }
        Ok(offspring)
    } else {
        let mut rng1 = rng; rng1.jump(1);
        let mut rng2 = rng; rng2.jump(2);
        let mut offspring: Offspring<G> = Vec::with_capacity(parents.len() * parents[0].len());
        let mid_point = parents.len() / 2;
        let mut parents = parents;
        let r_slice = parents.drain(mid_point..).collect();
        let l_slice = parents;
        let (left, right) = rayon::join(|| par_breed_offspring(l_slice, breeder, mutator, rng1),
                                        || par_breed_offspring(r_slice, breeder, mutator, rng2));
        match left {
            Ok(mut children) => offspring.append(&mut children),
            Err(error) => return Err(error),
        }
        match right {
            Ok(mut children) => offspring.append(&mut children),
            Err(error) => return Err(error),
        }
        Ok(offspring)
    }
}
