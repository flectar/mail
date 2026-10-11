/*
 * Copyright Stalwart Labs LLC See the COPYING
 * file at the top-level directory of this distribution.
 *
 * Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
 * https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
 * <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
 * option. This file may not be copied, modified, or distributed
 * except according to those terms.
 */

use serde::{Deserialize, Serialize};

use crate::Method;

use super::{request::ResultReference, Object, RequestParams};

pub trait QueryObject: Object {
    type QueryArguments: Default + Serialize;
    type Filter: Serialize;
    type Sort: Serialize;
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryRequest<O: QueryObject> {
    #[serde(skip)]
    method: (Method, usize),

    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "filter")]
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter<O::Filter>>,

    #[serde(rename = "sort")]
    #[serde(skip_serializing_if = "Option::is_none")]
    sort: Option<Vec<Comparator<O::Sort>>>,

    #[serde(rename = "position")]
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<i32>,

    #[serde(rename = "anchor")]
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor: Option<String>,

    #[serde(rename = "anchorOffset")]
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor_offset: Option<i32>,

    #[serde(rename = "limit")]
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,

    #[serde(rename = "calculateTotal")]
    #[serde(skip_serializing_if = "Option::is_none")]
    calculate_total: Option<bool>,

    #[serde(flatten)]
    arguments: O::QueryArguments,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Filter<T> {
    FilterOperator(FilterOperator<T>),
    FilterCondition(T),
}

#[derive(Debug, Clone, Serialize)]
pub struct FilterOperator<T> {
    operator: Operator,
    conditions: Vec<Filter<T>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Operator {
    #[serde(rename = "AND")]
    And,
    #[serde(rename = "OR")]
    Or,
    #[serde(rename = "NOT")]
    Not,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparator<A> {
    #[serde(rename = "isAscending")]
    is_ascending: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    collation: Option<String>,

    #[serde(flatten)]
    arguments: A,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryResponse {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "queryState")]
    query_state: String,

    #[serde(rename = "canCalculateChanges")]
    can_calculate_changes: Option<bool>,

    #[serde(rename = "position")]
    position: i32,

    #[serde(rename = "ids")]
    ids: Vec<String>,

    #[serde(rename = "total")]
    total: Option<usize>,

    #[serde(rename = "limit")]
    limit: Option<usize>,
}

impl<O: QueryObject> QueryRequest<O> {
    pub fn new(params: RequestParams) -> Self {
        QueryRequest {
            account_id: params.account_id,
            method: (params.method, params.call_id),
            filter: None,
            sort: None,
            position: None,
            anchor: None,
            anchor_offset: None,
            limit: None,
            calculate_total: None,
            arguments: O::QueryArguments::default(),
        }
    }

    pub fn account_id(&mut self, account_id: impl Into<String>) -> &mut Self {
        self.account_id = account_id.into();
        self
    }

    pub fn filter(&mut self, filter: impl Into<Filter<O::Filter>>) -> &mut Self {
        self.filter = Some(filter.into());
        self
    }

    pub fn sort(&mut self, sort: impl IntoIterator<Item = Comparator<O::Sort>>) -> &mut Self {
        self.sort = Some(sort.into_iter().collect());
        self
    }

    pub fn position(&mut self, position: i32) -> &mut Self {
        self.position = position.into();
        self
    }

    pub fn anchor(&mut self, anchor: impl Into<String>) -> &mut Self {
        self.anchor = Some(anchor.into());
        self
    }

    pub fn anchor_offset(&mut self, anchor_offset: i32) -> &mut Self {
        self.anchor_offset = anchor_offset.into();
        self
    }

    pub fn limit(&mut self, limit: usize) -> &mut Self {
        self.limit = Some(limit);
        self
    }

    pub fn calculate_total(&mut self, calculate_total: bool) -> &mut Self {
        self.calculate_total = Some(calculate_total);
        self
    }

    pub fn arguments(&mut self) -> &mut O::QueryArguments {
        &mut self.arguments
    }

    pub fn result_reference(&self) -> ResultReference {
        ResultReference::new(self.method.0, self.method.1, "/ids")
    }
}

impl QueryResponse {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn id(&self, pos: usize) -> &str {
        self.ids[pos].as_str()
    }

    pub fn take_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut self.ids)
    }

    pub fn total(&self) -> Option<usize> {
        self.total
    }

    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    pub fn position(&self) -> i32 {
        self.position
    }

    pub fn take_query_state(&mut self) -> String {
        std::mem::take(&mut self.query_state)
    }

    pub fn query_state(&self) -> &str {
        &self.query_state
    }

    pub fn can_calculate_changes(&self) -> bool {
        self.can_calculate_changes.unwrap_or(false)
    }
}

impl<A> Comparator<A> {
    pub fn new(arguments: A) -> Self {
        Comparator {
            is_ascending: true,
            collation: None,
            arguments,
        }
    }

    pub fn descending(mut self) -> Self {
        self.is_ascending = false;
        self
    }

    pub fn ascending(mut self) -> Self {
        self.is_ascending = true;
        self
    }

    pub fn is_ascending(mut self, is_ascending: bool) -> Self {
        self.is_ascending = is_ascending;
        self
    }

    pub fn collation(mut self, collation: String) -> Self {
        self.collation = Some(collation);
        self
    }
}

impl<T> From<FilterOperator<T>> for Filter<T> {
    fn from(filter: FilterOperator<T>) -> Self {
        Filter::FilterOperator(filter)
    }
}

impl<T> From<T> for Filter<T> {
    fn from(filter: T) -> Self {
        Filter::FilterCondition(filter)
    }
}

impl<T> Filter<T> {
    pub fn operator(operator: Operator, conditions: Vec<Filter<T>>) -> Self {
        Filter::FilterOperator(FilterOperator {
            operator,
            conditions,
        })
    }

    pub fn and<U, V>(conditions: U) -> Self
    where
        U: IntoIterator<Item = V>,
        V: Into<Filter<T>>,
    {
        Filter::FilterOperator(FilterOperator {
            operator: Operator::And,
            conditions: conditions.into_iter().map(|t| t.into()).collect(),
        })
    }

    pub fn or<U, V>(conditions: U) -> Self
    where
        U: IntoIterator<Item = V>,
        V: Into<Filter<T>>,
    {
        Filter::FilterOperator(FilterOperator {
            operator: Operator::Or,
            conditions: conditions.into_iter().map(|t| t.into()).collect(),
        })
    }

    pub fn not<U, V>(conditions: U) -> Self
    where
        U: IntoIterator<Item = V>,
        V: Into<Filter<T>>,
    {
        Filter::FilterOperator(FilterOperator {
            operator: Operator::Not,
            conditions: conditions.into_iter().map(|t| t.into()).collect(),
        })
    }
}
