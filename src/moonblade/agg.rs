use std::collections::HashMap;
use std::rc::Rc;

use csv::ByteRecord;

use super::error::{CallError, ConcretizationError, EvaluationError, SpecifiedCallError};
use super::interpreter::{concretize_expression, eval_expr, ConcreteArgument};
use super::parser::{parse_aggregations, Aggregation, Aggregations};
use super::types::{DynamicNumber, DynamicValue, HeadersIndex, Variables};

#[derive(Debug, Clone)]
struct Count {
    current: usize,
}

impl Count {
    fn new() -> Self {
        Self { current: 0 }
    }

    fn clear(&mut self) {
        self.current = 0;
    }

    fn add(&mut self) {
        self.current += 1;
    }

    fn get(&self) -> usize {
        self.current
    }
}

#[derive(Debug, Clone)]
struct AllAny {
    all: bool,
    any: bool,
}

impl AllAny {
    fn new() -> Self {
        Self {
            all: true,
            any: false,
        }
    }

    fn clear(&mut self) {
        self.all = true;
        self.any = false;
    }

    fn add(&mut self, new_bool: bool) {
        self.all = self.all && new_bool;
        self.any = self.any || new_bool;
    }

    fn all(&self) -> bool {
        self.all
    }

    fn any(&self) -> bool {
        self.any
    }
}

#[derive(Debug, Clone)]
struct FirstLast {
    first: Option<(usize, Rc<DynamicValue>)>,
    last: Option<(usize, Rc<DynamicValue>)>,
}

impl FirstLast {
    fn new() -> Self {
        Self {
            first: None,
            last: None,
        }
    }

    fn clear(&mut self) {
        self.first = None;
        self.last = None;
    }

    fn add(&mut self, index: usize, next_value: &Rc<DynamicValue>) {
        if self.first.is_none() {
            self.first = Some((index, next_value.clone()));
        }

        self.last = Some((index, next_value.clone()));
    }

    fn first(&self) -> Option<DynamicValue> {
        self.first.as_ref().map(|p| p.1.as_ref().clone())
    }

    fn last(&self) -> Option<DynamicValue> {
        self.last.as_ref().map(|p| p.1.as_ref().clone())
    }
}

#[derive(Debug, Clone)]
struct Sum {
    current: DynamicNumber,
}

impl Sum {
    fn new() -> Self {
        Self {
            current: DynamicNumber::Integer(0),
        }
    }

    fn clear(&mut self) {
        self.current = DynamicNumber::Integer(0);
    }

    // TODO: implement kahan-babushka summation from https://github.com/simple-statistics/simple-statistics/blob/main/src/sum.js
    fn add(&mut self, value: &DynamicNumber) {
        match &mut self.current {
            DynamicNumber::Float(a) => match value {
                DynamicNumber::Float(b) => *a += b,
                DynamicNumber::Integer(b) => *a += *b as f64,
            },
            DynamicNumber::Integer(a) => match value {
                DynamicNumber::Float(b) => self.current = DynamicNumber::Float((*a as f64) + b),
                DynamicNumber::Integer(b) => *a += b,
            },
        };
    }

    fn get(&self) -> DynamicNumber {
        self.current
    }
}

#[derive(Debug, Clone)]
struct Extent {
    extent: Option<(DynamicNumber, DynamicNumber)>,
}

impl Extent {
    fn new() -> Self {
        Self { extent: None }
    }

    fn clear(&mut self) {
        self.extent = None;
    }

    fn add(&mut self, value: DynamicNumber) {
        match &mut self.extent {
            None => self.extent = Some((value, value)),
            Some((min, max)) => {
                if value < *min {
                    *min = value;
                }

                if value > *max {
                    *max = value;
                }
            }
        }
    }

    fn min(&self) -> Option<DynamicNumber> {
        self.extent.map(|e| e.0)
    }

    fn max(&self) -> Option<DynamicNumber> {
        self.extent.map(|e| e.1)
    }
}

#[derive(Debug, Clone)]
struct LexicographicExtent {
    extent: Option<(String, String)>,
}

impl LexicographicExtent {
    fn new() -> Self {
        Self { extent: None }
    }

    fn clear(&mut self) {
        self.extent = None;
    }

    fn add(&mut self, value: &str) {
        match &mut self.extent {
            None => self.extent = Some((value.to_string(), value.to_string())),
            Some((min, max)) => {
                if value < min.as_str() {
                    *min = value.to_string();
                }

                if value > max.as_str() {
                    *max = value.to_string();
                }
            }
        }
    }

    fn first(&self) -> Option<String> {
        self.extent.as_ref().map(|e| e.0.clone())
    }

    fn last(&self) -> Option<String> {
        self.extent.as_ref().map(|e| e.1.clone())
    }
}

#[derive(Debug, Clone)]
enum MedianType {
    Interpolation,
    Low,
    High,
}

#[derive(Debug, Clone)]
struct Numbers {
    numbers: Vec<DynamicNumber>,
}

impl Numbers {
    fn new() -> Self {
        Self {
            numbers: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.numbers.clear();
    }

    fn add(&mut self, number: DynamicNumber) {
        self.numbers.push(number);
    }

    // TODO: par_finalize
    fn finalize(&mut self) {
        self.numbers.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }

    fn median(&self, median_type: &MedianType) -> Option<DynamicNumber> {
        let count = self.numbers.len();

        if count == 0 {
            return None;
        }

        let median = match median_type {
            MedianType::Low => {
                let mut midpoint = count / 2;

                if count % 2 == 0 {
                    midpoint -= 1;
                }

                self.numbers[midpoint]
            }
            MedianType::High => {
                let midpoint = count / 2;

                self.numbers[midpoint]
            }
            MedianType::Interpolation => {
                let midpoint = count / 2;

                if count % 2 == 1 {
                    self.numbers[midpoint]
                } else {
                    let down = &self.numbers[midpoint - 1];
                    let up = &self.numbers[midpoint];

                    (*down + *up) / DynamicNumber::Float(2.0)
                }
            }
        };

        Some(median)
    }
}

#[derive(Debug, Clone)]
struct Frequencies {
    counter: HashMap<String, usize>,
}

impl Frequencies {
    fn new() -> Self {
        Self {
            counter: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.counter.clear();
    }

    fn add(&mut self, value: String) {
        self.counter
            .entry(value)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    fn mode(&self) -> Option<String> {
        let mut max: Option<(usize, &String)> = None;

        for (key, count) in self.counter.iter() {
            max = match max {
                None => Some((*count, key)),
                Some(entry) => {
                    if (*count, key) > entry {
                        Some((*count, key))
                    } else {
                        max
                    }
                }
            }
        }

        max.map(|(_, key)| key.to_string())
    }

    fn cardinality(&self) -> usize {
        self.counter.len()
    }
}

// NOTE: this is an implementation of Welford's online algorithm
#[derive(Debug, Clone)]
struct Welford {
    count: usize,
    mean: f64,
    m2: f64,
}

impl Welford {
    fn new() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }

    fn clear(&mut self) {
        self.count = 0;
        self.mean = 0.0;
        self.m2 = 0.0;
    }

    fn add(&mut self, value: f64) {
        let (mut count, mut mean, mut m2) = (self.count, self.mean, self.m2);
        count += 1;
        let delta = value - mean;
        mean += delta / count as f64;
        let delta2 = value - mean;
        m2 += delta * delta2;

        self.count = count;
        self.mean = mean;
        self.m2 = m2;
    }

    fn mean(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }

        Some(self.mean)
    }

    fn variance(&self) -> Option<f64> {
        if self.count < 1 {
            return None;
        }

        Some(self.m2 / self.count as f64)
    }

    fn sample_variance(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }

        Some(self.m2 / (self.count - 1) as f64)
    }

    fn stdev(&self) -> Option<f64> {
        self.variance().map(|v| v.sqrt())
    }

    fn sample_stdev(&self) -> Option<f64> {
        self.sample_variance().map(|v| v.sqrt())
    }
}

macro_rules! build_aggregation_method_enum {
    ($($variant: ident,)+) => {
        #[derive(Debug, Clone)]
        enum Aggregator {
            $(
                $variant($variant),
            )+
        }

        impl Aggregator {
            fn clear(&mut self) {
                match self {
                    $(
                        Self::$variant(inner) => inner.clear(),
                    )+
                };
            }

            fn finalize(&mut self) {
                match self {
                    Self::Numbers(inner) => {
                        inner.finalize();
                    }
                    _ => (),
                }
            }
        }
    };
}

build_aggregation_method_enum!(
    AllAny,
    Count,
    Extent,
    FirstLast,
    LexicographicExtent,
    Frequencies,
    Numbers,
    Sum,
    Welford,
);

impl Aggregator {
    fn get_final_value(&self, method: &ConcreteAggregationMethod) -> DynamicValue {
        match (self, method) {
            (Self::AllAny(inner), ConcreteAggregationMethod::All) => {
                DynamicValue::from(inner.all())
            }
            (Self::AllAny(inner), ConcreteAggregationMethod::Any) => {
                DynamicValue::from(inner.any())
            }
            (Self::Frequencies(inner), ConcreteAggregationMethod::Cardinality) => {
                DynamicValue::from(inner.cardinality())
            }
            (Self::Count(inner), ConcreteAggregationMethod::Count) => {
                DynamicValue::from(inner.get())
            }
            (Self::FirstLast(inner), ConcreteAggregationMethod::First) => {
                DynamicValue::from(inner.first())
            }
            (Self::FirstLast(inner), ConcreteAggregationMethod::Last) => {
                DynamicValue::from(inner.last())
            }
            (Self::LexicographicExtent(inner), ConcreteAggregationMethod::LexFirst) => {
                DynamicValue::from(inner.first())
            }
            (Self::LexicographicExtent(inner), ConcreteAggregationMethod::LexLast) => {
                DynamicValue::from(inner.last())
            }
            (Self::Extent(inner), ConcreteAggregationMethod::Min) => {
                DynamicValue::from(inner.min())
            }
            (Self::Welford(inner), ConcreteAggregationMethod::Mean) => {
                DynamicValue::from(inner.mean())
            }
            (Self::Numbers(inner), ConcreteAggregationMethod::Median(median_type)) => {
                DynamicValue::from(inner.median(median_type))
            }
            (Self::Extent(inner), ConcreteAggregationMethod::Max) => {
                DynamicValue::from(inner.max())
            }
            (Self::Frequencies(inner), ConcreteAggregationMethod::Mode) => {
                DynamicValue::from(inner.mode())
            }
            (Self::Sum(inner), ConcreteAggregationMethod::Sum) => DynamicValue::from(inner.get()),
            (Self::Welford(inner), ConcreteAggregationMethod::VarPop) => {
                DynamicValue::from(inner.variance())
            }
            (Self::Welford(inner), ConcreteAggregationMethod::VarSample) => {
                DynamicValue::from(inner.sample_variance())
            }
            (Self::Welford(inner), ConcreteAggregationMethod::StddevPop) => {
                DynamicValue::from(inner.stdev())
            }
            (Self::Welford(inner), ConcreteAggregationMethod::StddevSample) => {
                DynamicValue::from(inner.sample_stdev())
            }
            _ => unreachable!(),
        }
    }
}

// NOTE: at the beginning I was using a struct that would look like this:
// struct Aggregator {
//     count: Option<Count>,
//     sum: Option<Sum>,
// }

// But this has the downside of allocating a lot of memory for each Aggregator
// instances, and since we need to instantiate one Aggregator per group when
// aggregating per group, this would cost quite a lot of memory for no good
// reason. We can of course store a list of CSV rows per group but this would
// also cost O(n) memory (n being the size of target CSV file), whereas we
// actually only need O(1) memory per group, i.e. O(g) for most aggregation
// methods (e.g. sum, mean etc.).

// Note that we can wrap the inner aggregators in a Box to reduce the memory
// footprint. But this will still increase each time we add a new aggregation
// function family, which is far from ideal.

// The current solution relies on an enum of aggregation method `AggregationMethod`
// and an `Aggregator` struct which is basically wrapping only a vector of
// said enum, making it as light as possible. This is somewhat verbose however
// and we could rely on macros to help with this if needed.

// NOTE: this aggregator actively combines and matches different generic
// aggregation schemes and never repeats itself. For instance, mean will be
// inferred from aggregating sum and count. Also if the user asks for both
// sum and mean, the sum will only be aggregated once.

#[derive(Debug, Clone)]
struct CompositeAggregator {
    methods: Vec<Aggregator>,
}

impl CompositeAggregator {
    fn new() -> Self {
        Self {
            methods: Vec::new(),
        }
    }

    fn clear(&mut self) {
        for method in self.methods.iter_mut() {
            method.clear();
        }
    }

    fn add_method(&mut self, method: &ConcreteAggregationMethod) -> usize {
        macro_rules! upsert_aggregator {
            ($variant: ident) => {
                match self.methods.iter().position(|item| match item {
                    Aggregator::$variant(_) => true,
                    _ => false,
                }) {
                    Some(idx) => idx,
                    None => {
                        let idx = self.methods.len();
                        self.methods.push(Aggregator::$variant($variant::new()));
                        idx
                    }
                }
            };
        }

        match method {
            ConcreteAggregationMethod::All | ConcreteAggregationMethod::Any => {
                upsert_aggregator!(AllAny)
            }
            ConcreteAggregationMethod::Count => {
                upsert_aggregator!(Count)
            }
            ConcreteAggregationMethod::Min | ConcreteAggregationMethod::Max => {
                upsert_aggregator!(Extent)
            }
            ConcreteAggregationMethod::First | ConcreteAggregationMethod::Last => {
                upsert_aggregator!(FirstLast)
            }
            ConcreteAggregationMethod::LexFirst | ConcreteAggregationMethod::LexLast => {
                upsert_aggregator!(LexicographicExtent)
            }
            ConcreteAggregationMethod::Median(_) => {
                upsert_aggregator!(Numbers)
            }
            ConcreteAggregationMethod::Mode | ConcreteAggregationMethod::Cardinality => {
                upsert_aggregator!(Frequencies)
            }
            ConcreteAggregationMethod::Sum => {
                upsert_aggregator!(Sum)
            }
            ConcreteAggregationMethod::Mean
            | ConcreteAggregationMethod::VarPop
            | ConcreteAggregationMethod::VarSample
            | ConcreteAggregationMethod::StddevPop
            | ConcreteAggregationMethod::StddevSample => {
                upsert_aggregator!(Welford)
            }
        }
    }

    fn process_value(
        &mut self,
        index: usize,
        value_opt: Option<DynamicValue>,
    ) -> Result<(), CallError> {
        let value_opt = value_opt.map(Rc::new);

        for method in self.methods.iter_mut() {
            match value_opt.as_ref() {
                Some(value) => match method {
                    Aggregator::AllAny(allany) => {
                        allany.add(value.is_truthy());
                    }
                    Aggregator::Count(count) => {
                        if !value.is_nullish() {
                            count.add();
                        }
                    }
                    Aggregator::Extent(extent) => {
                        extent.add(value.try_as_number()?);
                    }
                    Aggregator::FirstLast(firstlast) => {
                        if !value.is_nullish() {
                            firstlast.add(index, value);
                        }
                    }
                    Aggregator::LexicographicExtent(extent) => {
                        extent.add(&value.try_as_str()?);
                    }
                    Aggregator::Frequencies(frequencies) => {
                        frequencies.add(value.try_as_str()?.into_owned());
                    }
                    Aggregator::Numbers(numbers) => {
                        numbers.add(value.try_as_number()?);
                    }
                    Aggregator::Sum(sum) => {
                        sum.add(&value.try_as_number()?);
                    }
                    Aggregator::Welford(variance) => {
                        variance.add(value.try_as_f64()?);
                    }
                },
                None => match method {
                    Aggregator::Count(count) => {
                        count.add();
                    }
                    _ => unreachable!(),
                },
            }
        }

        Ok(())
    }

    fn finalize(&mut self) {
        for method in self.methods.iter_mut() {
            method.finalize();
        }
    }

    fn get_final_value(&self, handle: usize, method: &ConcreteAggregationMethod) -> DynamicValue {
        self.methods[handle].get_final_value(method)
    }
}

fn validate_aggregation_function_arity(
    aggregation: &Aggregation,
) -> Result<(), ConcretizationError> {
    let arity = aggregation.args.len();

    match aggregation.func_name.as_str() {
        "count" => {
            if !(0..=1).contains(&arity) {
                Err(ConcretizationError::from_invalid_range_arity(
                    aggregation.func_name.clone(),
                    0..=1,
                    arity,
                ))
            } else {
                Ok(())
            }
        }
        _ => {
            if arity != 1 {
                Err(ConcretizationError::from_invalid_arity(
                    aggregation.func_name.clone(),
                    1,
                    arity,
                ))
            } else {
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
enum ConcreteAggregationMethod {
    All,
    Any,
    Cardinality,
    Count,
    First,
    Last,
    LexFirst,
    LexLast,
    Min,
    Max,
    Mean,
    Median(MedianType),
    Mode,
    Sum,
    VarPop,
    VarSample,
    StddevPop,
    StddevSample,
}

impl ConcreteAggregationMethod {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "all" => Self::All,
            "any" => Self::Any,
            "cardinality" => Self::Cardinality,
            "count" => Self::Count,
            "first" => Self::First,
            "last" => Self::Last,
            "lex_first" => Self::LexFirst,
            "lex_last" => Self::LexLast,
            "min" => Self::Min,
            "max" => Self::Max,
            "avg" | "mean" => Self::Mean,
            "median" => Self::Median(MedianType::Interpolation),
            "median_high" => Self::Median(MedianType::High),
            "median_low" => Self::Median(MedianType::Low),
            "mode" => Self::Mode,
            "var" | "var_pop" => Self::VarPop,
            "var_sample" => Self::VarSample,
            "stddev" | "stddev_pop" => Self::StddevPop,
            "stddev_sample" => Self::StddevSample,
            "sum" => Self::Sum,
            _ => return None,
        })
    }
}

#[derive(Debug)]
struct ConcreteAggregation {
    agg_name: String,
    method: ConcreteAggregationMethod,
    expr: Option<ConcreteArgument>,
    expr_key: String,
    // args: Vec<ConcreteArgument>,
}

type ConcreteAggregations = Vec<ConcreteAggregation>;

fn concretize_aggregations(
    aggregations: Aggregations,
    headers: &ByteRecord,
) -> Result<ConcreteAggregations, ConcretizationError> {
    let mut concrete_aggregations = ConcreteAggregations::new();

    for aggregation in aggregations {
        validate_aggregation_function_arity(&aggregation)?;

        let expr = aggregation
            .args
            .get(0)
            .map(|arg| concretize_expression(arg.clone(), headers))
            .transpose()?;

        let mut args: Vec<ConcreteArgument> = Vec::new();

        for arg in aggregation.args.into_iter().skip(1) {
            args.push(concretize_expression(arg, headers)?);
        }

        if let Some(method) = ConcreteAggregationMethod::parse(&aggregation.func_name) {
            let concrete_aggregation = ConcreteAggregation {
                agg_name: aggregation.agg_name,
                method,
                expr_key: aggregation.expr_key,
                expr,
                // args,
            };

            concrete_aggregations.push(concrete_aggregation);
        } else {
            return Err(ConcretizationError::UnknownFunction(aggregation.func_name));
        }
    }

    Ok(concrete_aggregations)
}

fn prepare(code: &str, headers: &ByteRecord) -> Result<ConcreteAggregations, ConcretizationError> {
    let parsed_aggregations =
        parse_aggregations(code).map_err(|_| ConcretizationError::ParseError(code.to_string()))?;

    concretize_aggregations(parsed_aggregations, headers)
}

// NOTE: each execution unit is iterated upon linearly to aggregate values
// all while running a minimum number of operations (batched by 1. expression
// keys and 2. composite aggregation atom).
#[derive(Debug)]
struct PlannerExecutionUnit {
    expr_key: String,
    expr: Option<ConcreteArgument>,
    aggregator_blueprint: CompositeAggregator,
}

// NOTE: output unit are aligned with the list of concrete aggregations and
// offer a way to navigate the expression key indexation layer, then the
// composite aggregation layer.
#[derive(Debug)]
struct PlannerOutputUnit {
    expr_index: usize,
    aggregator_index: usize,
    agg_name: String,
    agg_method: ConcreteAggregationMethod,
}

#[derive(Debug)]
struct ConcreteAggregationPlanner {
    execution_plan: Vec<PlannerExecutionUnit>,
    output_plan: Vec<PlannerOutputUnit>,
}

impl From<ConcreteAggregations> for ConcreteAggregationPlanner {
    fn from(aggregations: ConcreteAggregations) -> Self {
        let mut execution_plan = Vec::<PlannerExecutionUnit>::new();
        let mut output_plan = Vec::<PlannerOutputUnit>::with_capacity(aggregations.len());

        for agg in aggregations {
            if let Some(expr_index) = execution_plan
                .iter()
                .position(|unit| unit.expr_key == agg.expr_key)
            {
                let aggregator_index = execution_plan[expr_index]
                    .aggregator_blueprint
                    .add_method(&agg.method);

                output_plan.push(PlannerOutputUnit {
                    expr_index,
                    aggregator_index,
                    agg_name: agg.agg_name,
                    agg_method: agg.method,
                });
            } else {
                let expr_index = execution_plan.len();
                let mut aggregator_blueprint = CompositeAggregator::new();
                let aggregator_index = aggregator_blueprint.add_method(&agg.method);

                execution_plan.push(PlannerExecutionUnit {
                    expr_key: agg.expr_key,
                    expr: agg.expr,
                    aggregator_blueprint,
                });

                output_plan.push(PlannerOutputUnit {
                    expr_index,
                    aggregator_index,
                    agg_name: agg.agg_name,
                    agg_method: agg.method,
                });
            }
        }

        Self {
            execution_plan,
            output_plan,
        }
    }
}

impl ConcreteAggregationPlanner {
    fn instantiate_aggregators(&self) -> Vec<CompositeAggregator> {
        self.execution_plan
            .iter()
            .map(|unit| unit.aggregator_blueprint.clone())
            .collect()
    }

    fn headers(&self) -> impl Iterator<Item = &[u8]> {
        self.output_plan.iter().map(|unit| unit.agg_name.as_bytes())
    }

    fn results<'a>(
        &'a self,
        aggregators: &'a [CompositeAggregator],
    ) -> impl Iterator<Item = DynamicValue> + 'a {
        self.output_plan.iter().map(move |unit| {
            aggregators[unit.expr_index].get_final_value(unit.aggregator_index, &unit.agg_method)
        })
    }
}

fn run_with_record_on_aggregators(
    planner: &ConcreteAggregationPlanner,
    aggregators: &mut Vec<CompositeAggregator>,
    index: usize,
    record: &ByteRecord,
    headers_index: &HeadersIndex,
    variables: &Variables,
) -> Result<(), EvaluationError> {
    for (unit, aggregator) in planner.execution_plan.iter().zip(aggregators) {
        let value = match &unit.expr {
            None => None,
            Some(expr) => Some(eval_expr(expr, record, headers_index, variables)?),
        };

        aggregator.process_value(index, value).map_err(|err| {
            EvaluationError::Call(SpecifiedCallError {
                reason: err,
                function_name: format!("<agg-expr: {}>", unit.expr_key),
            })
        })?;
    }

    Ok(())
}

#[derive(Debug)]
pub struct AggregationProgram<'a> {
    aggregators: Vec<CompositeAggregator>,
    planner: ConcreteAggregationPlanner,
    headers_index: HeadersIndex,
    variables: Variables<'a>,
}

impl<'a> AggregationProgram<'a> {
    pub fn parse(code: &str, headers: &ByteRecord) -> Result<Self, ConcretizationError> {
        let concrete_aggregations = prepare(code, headers)?;
        let planner = ConcreteAggregationPlanner::from(concrete_aggregations);
        let aggregators = planner.instantiate_aggregators();

        Ok(Self {
            planner,
            aggregators,
            headers_index: HeadersIndex::from_headers(headers),
            variables: Variables::new(),
        })
    }

    pub fn clear(&mut self) {
        for aggregator in self.aggregators.iter_mut() {
            aggregator.clear()
        }
    }

    pub fn run_with_record(
        &mut self,
        index: usize,
        record: &ByteRecord,
    ) -> Result<(), EvaluationError> {
        run_with_record_on_aggregators(
            &self.planner,
            &mut self.aggregators,
            index,
            record,
            &self.headers_index,
            &self.variables,
        )
    }

    pub fn headers(&self) -> impl Iterator<Item = &[u8]> {
        self.planner.headers()
    }

    pub fn finalize(&mut self) -> ByteRecord {
        for aggregator in self.aggregators.iter_mut() {
            aggregator.finalize();
        }

        let mut record = ByteRecord::new();

        for value in self.planner.results(&self.aggregators) {
            record.push_field(&value.serialize_as_bytes());
        }

        record
    }
}

#[derive(Debug)]
pub struct GroupAggregationProgram<'a> {
    planner: ConcreteAggregationPlanner,
    groups: HashMap<Vec<u8>, Vec<CompositeAggregator>>,
    headers_index: HeadersIndex,
    variables: Variables<'a>,
}

impl<'a> GroupAggregationProgram<'a> {
    pub fn parse(code: &str, headers: &ByteRecord) -> Result<Self, ConcretizationError> {
        let concrete_aggregations = prepare(code, headers)?;
        let planner = ConcreteAggregationPlanner::from(concrete_aggregations);

        Ok(Self {
            planner,
            groups: HashMap::new(),
            headers_index: HeadersIndex::from_headers(headers),
            variables: Variables::new(),
        })
    }

    pub fn run_with_record(
        &mut self,
        group: Vec<u8>,
        index: usize,
        record: &ByteRecord,
    ) -> Result<(), EvaluationError> {
        let planner = &self.planner;

        let aggregators = self
            .groups
            .entry(group)
            .or_insert_with(|| planner.instantiate_aggregators());

        run_with_record_on_aggregators(
            &self.planner,
            aggregators,
            index,
            record,
            &self.headers_index,
            &self.variables,
        )
    }

    pub fn headers(&self) -> impl Iterator<Item = &[u8]> {
        self.planner.headers()
    }

    pub fn into_byte_records(self) -> impl Iterator<Item = (Vec<u8>, ByteRecord)> + 'a {
        let planner = self.planner;

        self.groups
            .into_iter()
            .map(move |(group, mut aggregators)| {
                for aggregator in aggregators.iter_mut() {
                    aggregator.finalize();
                }

                let mut record = ByteRecord::new();

                for value in planner.results(&aggregators) {
                    record.push_field(&value.serialize_as_bytes());
                }

                (group, record)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl From<Vec<usize>> for Numbers {
        fn from(values: Vec<usize>) -> Self {
            let mut numbers = Self::new();

            for n in values {
                numbers.add(DynamicNumber::Integer(n as i64));
            }

            numbers
        }
    }

    #[test]
    fn test_median_types() {
        let odd = vec![1, 3, 5];
        let even = vec![1, 2, 6, 7];

        let mut no_numbers = Numbers::new();
        let mut lone_numbers = Numbers::from(vec![8]);
        let mut odd_numbers = Numbers::from(odd);
        let mut even_numbers = Numbers::from(even);

        no_numbers.finalize();
        lone_numbers.finalize();
        odd_numbers.finalize();
        even_numbers.finalize();

        // Low
        assert_eq!(no_numbers.median(&MedianType::Low), None);

        assert_eq!(
            lone_numbers.median(&MedianType::Low),
            Some(DynamicNumber::Integer(8))
        );

        assert_eq!(
            odd_numbers.median(&MedianType::Low),
            Some(DynamicNumber::Integer(3))
        );

        assert_eq!(
            even_numbers.median(&MedianType::Low),
            Some(DynamicNumber::Integer(2))
        );

        // High
        assert_eq!(no_numbers.median(&MedianType::High), None);

        assert_eq!(
            lone_numbers.median(&MedianType::High),
            Some(DynamicNumber::Integer(8))
        );

        assert_eq!(
            odd_numbers.median(&MedianType::High),
            Some(DynamicNumber::Integer(3))
        );

        assert_eq!(
            even_numbers.median(&MedianType::High),
            Some(DynamicNumber::Integer(6))
        );

        // High
        assert_eq!(no_numbers.median(&MedianType::Interpolation), None);

        assert_eq!(
            lone_numbers.median(&MedianType::Interpolation),
            Some(DynamicNumber::Integer(8))
        );

        assert_eq!(
            odd_numbers.median(&MedianType::Interpolation),
            Some(DynamicNumber::Integer(3))
        );

        assert_eq!(
            even_numbers.median(&MedianType::Interpolation),
            Some(DynamicNumber::Float(4.0))
        );
    }

    // #[test]
    // fn test_planner() {
    //     let mut headers = ByteRecord::new();
    //     headers.push_field(b"A");
    //     headers.push_field(b"B");
    //     headers.push_field(b"C");

    //     let agg = parse_aggregations("mean(A), var(A), sum(B), last(A), first(C)").unwrap();
    //     let agg = concretize_aggregations(agg, &headers).unwrap();

    //     let planner = ConcreteAggregationPlanner::from(agg);

    //     dbg!(planner);
    // }
}
