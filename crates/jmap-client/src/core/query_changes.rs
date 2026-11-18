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

use super::{
    query::{Comparator, Filter, QueryObject},
    RequestParams,
};

#[derive(Debug, Clone, Serialize)]
pub struct QueryChangesRequest<O: QueryObject> {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "filter")]
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter<O::Filter>>,

    #[serde(rename = "sort")]
    #[serde(skip_serializing_if = "Option::is_none")]
    sort: Option<Vec<Comparator<O::Sort>>>,

    #[serde(rename = "sinceQueryState")]
    since_query_state: String,

    #[serde(rename = "maxChanges")]
    #[serde(skip_serializing_if = "Option::is_none")]
    max_changes: Option<usize>,

    #[serde(rename = "upToId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    up_to_id: Option<String>,

    #[serde(rename = "calculateTotal")]
    calculate_total: bool,

    #[serde(flatten)]
    arguments: O::QueryArguments,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueryChangesResponse {
    #[serde(rename = "accountId")]
    account_id: String,
    #[serde(rename = "oldQueryState")]
    old_query_state: String,
    #[serde(rename = "newQueryState")]
    new_query_state: String,
    #[serde(rename = "total")]
    total: Option<usize>,
    #[serde(rename = "removed")]
    removed: Vec<String>,
    #[serde(rename = "added")]
    added: Vec<AddedItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddedItem {
    id: String,
    index: usize,
}

impl<O: QueryObject> QueryChangesRequest<O> {
    pub fn new(params: RequestParams, since_query_state: String) -> Self {
        QueryChangesRequest {
            account_id: params.account_id,
            filter: None,
            sort: None,
            since_query_state,
            max_changes: None,
            up_to_id: None,
            calculate_total: false,
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

    pub fn max_changes(&mut self, max_changes: usize) -> &mut Self {
        self.max_changes = Some(max_changes);
        self
    }

    pub fn up_to_id(&mut self, up_to_id: impl Into<String>) -> &mut Self {
        self.up_to_id = Some(up_to_id.into());
        self
    }

    pub fn calculate_total(&mut self, calculate_total: bool) -> &mut Self {
        self.calculate_total = calculate_total;
        self
    }

    pub fn arguments(&mut self) -> &mut O::QueryArguments {
        &mut self.arguments
    }
}

impl QueryChangesResponse {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn old_query_state(&self) -> &str {
        &self.old_query_state
    }

    pub fn new_query_state(&self) -> &str {
        &self.new_query_state
    }

    pub fn total(&self) -> Option<usize> {
        self.total
    }

    pub fn removed(&self) -> &[String] {
        &self.removed
    }

    pub fn added(&self) -> &[AddedItem] {
        &self.added
    }
}

impl AddedItem {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn index(&self) -> usize {
        self.index
    }
}
