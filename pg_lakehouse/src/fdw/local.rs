// Copyright (c) 2023-2024 Retake, Inc.
//
// This file is part of ParadeDB - Postgres for Search and Analytics
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use async_std::stream::StreamExt;
use async_std::task;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::prelude::DataFrame;
use object_store::local::LocalFileSystem;
use pgrx::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use supabase_wrappers::prelude::*;
use url::Url;

use crate::datafusion::context::ContextError;
use crate::datafusion::format::TableFormat;
use crate::datafusion::session::Session;
use crate::fdw::options::*;

use super::base::*;

#[wrappers_fdw(
    author = "ParadeDB",
    website = "https://github.com/paradedb/paradedb",
    error_type = "BaseFdwError"
)]
pub(crate) struct LocalFileFdw {
    dataframe: Option<DataFrame>,
    stream: Option<SendableRecordBatchStream>,
    current_batch: Option<RecordBatch>,
    current_batch_index: usize,
    target_columns: Vec<Column>,
}

impl BaseFdw for LocalFileFdw {
    fn register_object_store(
        url: &Url,
        format: TableFormat,
        _server_options: HashMap<String, String>,
        _user_mapping_options: HashMap<String, String>,
    ) -> Result<(), ContextError> {
        let object_store = match format {
            TableFormat::Delta => LocalFileSystem::new_with_prefix(Path::new(url.path()))?,
            _ => LocalFileSystem::new(),
        };

        let context = Session::session_context()?;

        // Create SessionContext with ObjectStore
        context
            .runtime_env()
            .register_object_store(url, Arc::new(object_store));

        Ok(())
    }

    fn get_current_batch(&self) -> Option<RecordBatch> {
        self.current_batch.clone()
    }

    fn get_current_batch_index(&self) -> usize {
        self.current_batch_index
    }

    fn get_target_columns(&self) -> Vec<Column> {
        self.target_columns.clone()
    }

    fn set_current_batch(&mut self, batch: Option<RecordBatch>) {
        self.current_batch = batch;
    }

    fn set_current_batch_index(&mut self, index: usize) {
        self.current_batch_index = index;
    }

    fn set_dataframe(&mut self, dataframe: DataFrame) {
        self.dataframe = Some(dataframe);
    }

    async fn create_stream(&mut self) -> Result<(), BaseFdwError> {
        if self.stream.is_none() {
            self.stream = Some(
                self.dataframe
                    .clone()
                    .ok_or(BaseFdwError::DataFrameNotFound)?
                    .execute_stream()
                    .await?,
            );
        }

        Ok(())
    }

    fn clear_stream(&mut self) {
        self.stream = None;
    }

    fn set_target_columns(&mut self, columns: &[Column]) {
        self.target_columns = columns.to_vec();
    }

    async fn get_next_batch(&mut self) -> Result<Option<RecordBatch>, BaseFdwError> {
        match self
            .stream
            .as_mut()
            .ok_or(BaseFdwError::StreamNotFound)?
            .next()
            .await
        {
            Some(Ok(batch)) => Ok(Some(batch)),
            None => Ok(None),
            Some(Err(err)) => Err(BaseFdwError::DataFusionError(err)),
        }
    }
}

impl ForeignDataWrapper<BaseFdwError> for LocalFileFdw {
    fn new(
        table_options: HashMap<String, String>,
        server_options: HashMap<String, String>,
        user_mapping_options: HashMap<String, String>,
    ) -> Result<Self, BaseFdwError> {
        let path = require_option(TableOption::Path.as_str(), &table_options)?;
        let format = require_option_or(TableOption::Format.as_str(), &table_options, "");

        LocalFileFdw::register_object_store(
            &Url::parse(path)?,
            TableFormat::from(format),
            server_options,
            user_mapping_options,
        )?;

        Ok(Self {
            dataframe: None,
            current_batch: None,
            current_batch_index: 0,
            stream: None,
            target_columns: Vec::new(),
        })
    }

    fn validator(
        opt_list: Vec<Option<String>>,
        catalog: Option<pg_sys::Oid>,
    ) -> Result<(), BaseFdwError> {
        if let Some(oid) = catalog {
            match oid {
                FOREIGN_DATA_WRAPPER_RELATION_ID => {}
                FOREIGN_SERVER_RELATION_ID => {}
                FOREIGN_TABLE_RELATION_ID => {
                    let valid_options: Vec<String> = TableOption::iter()
                        .map(|opt| opt.as_str().to_string())
                        .collect();

                    validate_options(opt_list.clone(), valid_options)?;

                    for opt in TableOption::iter() {
                        if opt.is_required() {
                            check_options_contain(&opt_list, opt.as_str())?;
                        }
                    }
                }
                unsupported => {
                    return Err(BaseFdwError::UnsupportedFdwOid(PgOid::from(unsupported)))
                }
            }
        }

        Ok(())
    }

    fn begin_scan(
        &mut self,
        _quals: &[Qual],
        columns: &[Column],
        _sorts: &[Sort],
        limit: &Option<Limit>,
        options: HashMap<String, String>,
    ) -> Result<(), BaseFdwError> {
        task::block_on(self.begin_scan_impl(_quals, columns, _sorts, limit, options))
    }

    fn iter_scan(&mut self, row: &mut Row) -> Result<Option<()>, BaseFdwError> {
        task::block_on(self.iter_scan_impl(row))
    }

    fn end_scan(&mut self) -> Result<(), BaseFdwError> {
        self.end_scan_impl()
    }
}
