use crate::id_mapper::id_mapper::IdMapper;
use crate::vector_storage::vector_storage::VectorStorage;
use crate::payload_storage::payload_storage::{PayloadStorage};
use crate::entry::entry_point::{SegmentEntry, Result, OperationError};
use crate::types::{Filter, PayloadKeyType, PayloadType, SeqNumberType, VectorElementType, PointIdType, PointOffsetType, SearchParams, ScoredPoint, TheMap, SegmentStats};
use crate::query_planner::query_planner::QueryPlanner;
use std::sync::Arc;
use atomic_refcell::AtomicRefCell;

/// Simple segment implementation
pub struct Segment {
    pub version: SeqNumberType,
    pub id_mapper: Arc<AtomicRefCell<dyn IdMapper>>,
    pub vector_storage: Arc<AtomicRefCell<dyn VectorStorage>>,
    pub payload_storage: Arc<AtomicRefCell<dyn PayloadStorage>>,
    /// User for writing only here.
    pub query_planner: Arc<AtomicRefCell<dyn QueryPlanner>>,
    pub appendable_flag: bool,
}


impl Segment {
    fn update_vector(&mut self,
                     old_iternal_id: PointOffsetType,
                     vector: &Vec<VectorElementType>,
    ) -> PointOffsetType {
        let payload = self.payload_storage.borrow_mut().drop(old_iternal_id);
        let new_internal_index = {
            let mut vector_storage = self.vector_storage.borrow_mut();
            vector_storage.delete(old_iternal_id);
            vector_storage.put_vector(vector)
        };
        match payload {
            Some(payload) => self.payload_storage
                .borrow_mut()
                .assign_all(new_internal_index, payload),
            None => ()
        }
        new_internal_index
    }

    fn skip_by_version(&mut self, op_num: SeqNumberType) -> bool {
        return if self.version > op_num {
            true
        } else {
            self.version = op_num;
            false
        }
    }

    fn lookup_internal_id(&self, point_id: PointIdType) -> Result<PointOffsetType> {
        let internal_id_opt = self.id_mapper.borrow().internal_id(point_id);
        match internal_id_opt {
            Some(internal_id) => Ok(internal_id),
            None => Err(OperationError::PointIdError { missed_point_id: point_id })
        }
    }
}


impl SegmentEntry for Segment {
    fn version(&self) -> SeqNumberType { self.version }

    fn is_appendable(&self) -> bool {
        return self.appendable_flag;
    }

    fn search(&self,
              vector: &Vec<VectorElementType>,
              filter: Option<&Filter>,
              top: usize,
              params: Option<&SearchParams>,
    ) -> Result<Vec<ScoredPoint>> {
        let expected_vector_dim = self.vector_storage.borrow().vector_dim();
        if expected_vector_dim != vector.len() {
            return Err(OperationError::WrongVector {
                expected_dim: expected_vector_dim,
                received_dim: vector.len(),
            });
        }

        let internal_result = self.query_planner.borrow().search(vector, filter, top, params);


        let id_mapper = self.id_mapper.borrow();
        let res = internal_result.iter()
            .map(|&scored_point_offset|
                (
                    ScoredPoint {
                        idx: id_mapper
                            .external_id(scored_point_offset.idx)
                            .unwrap_or_else(|| panic!("Corrupter id_mapper, no external value for {}", scored_point_offset.idx)),
                        score: scored_point_offset.score,
                    }
                )
            ).collect();
        return Ok(res);
    }

    fn upsert_point(&mut self, op_num: SeqNumberType, point_id: PointIdType, vector: &Vec<VectorElementType>) -> Result<bool> {
        if self.skip_by_version(op_num) { return Ok(false); }

        let vector_dim = self.vector_storage.borrow().vector_dim();
        if vector_dim != vector.len() {
            return Err(OperationError::WrongVector { expected_dim: vector_dim, received_dim: vector.len() });
        }

        let stored_internal_point = {
            let id_mapped = self.id_mapper.borrow();
            id_mapped.internal_id(point_id)
        };

        let (was_replaced, new_index) = match stored_internal_point {
            Some(existing_internal_id) =>
                (true, self.update_vector(existing_internal_id, vector)),
            None =>
                (false, self.vector_storage.borrow_mut().put_vector(vector))
        };

        self.id_mapper.borrow_mut().set_link(point_id, new_index);
        Ok(was_replaced)
    }

    fn delete_point(&mut self, op_num: SeqNumberType, point_id: PointIdType) -> Result<bool> {
        if self.skip_by_version(op_num) { return Ok(false); };
        let mut mapper = self.id_mapper.borrow_mut();
        let internal_id = mapper.internal_id(point_id);
        match internal_id {
            Some(internal_id) => {
                self.vector_storage.borrow_mut().delete(internal_id);
                mapper.drop(point_id);
                Ok(true)
            }
            None => Ok(false)
        }
    }

    fn set_full_payload(&mut self,
                        op_num: SeqNumberType,
                        point_id: PointIdType,
                        full_payload: TheMap<PayloadKeyType, PayloadType>,
    ) -> Result<bool> {
        if self.skip_by_version(op_num) { return Ok(false); };
        let internal_id = self.lookup_internal_id(point_id)?;
        self.payload_storage.borrow_mut().assign_all(internal_id, full_payload);
        Ok(true)
    }

    fn set_payload(&mut self,
                   op_num: SeqNumberType,
                   point_id: PointIdType,
                   key: &PayloadKeyType,
                   payload: PayloadType,
    ) -> Result<bool> {
        if self.skip_by_version(op_num) { return Ok(false); };
        let internal_id = self.lookup_internal_id(point_id)?;
        self.payload_storage.borrow_mut().assign(internal_id, key, payload);
        Ok(true)
    }

    fn delete_payload(&mut self, op_num: SeqNumberType, point_id: PointIdType, key: &PayloadKeyType) -> Result<bool> {
        if self.skip_by_version(op_num) { return Ok(false); };
        let internal_id = self.lookup_internal_id(point_id)?;
        self.payload_storage.borrow_mut().delete(internal_id, key);
        Ok(true)
    }

    fn clear_payload(&mut self, op_num: SeqNumberType, point_id: PointIdType) -> Result<bool> {
        if self.skip_by_version(op_num) { return Ok(false); };
        let internal_id = self.lookup_internal_id(point_id)?;
        self.payload_storage.borrow_mut().drop(internal_id);
        Ok(true)
    }

    fn vector(&self, point_id: PointIdType) -> Result<Vec<VectorElementType>> {
        let internal_id = self.lookup_internal_id(point_id)?;
        Ok(self.vector_storage.borrow().get_vector(internal_id).unwrap())
    }

    fn payload(&self, point_id: PointIdType) -> Result<TheMap<PayloadKeyType, PayloadType>> {
        let internal_id = self.lookup_internal_id(point_id)?;
        Ok(self.payload_storage.borrow().payload(internal_id))
    }

    fn has_point(&self, point_id: PointIdType) -> bool {
        self.id_mapper.borrow().internal_id(point_id).is_some()
    }

    fn vectors_count(&self) -> usize {
        self.vector_storage.borrow().vector_count()
    }

    fn info(&self) -> SegmentStats {
        SegmentStats {
            num_vectors: self.vectors_count(),
            num_deleted_vectors: self.vector_storage.borrow().deleted_count(),
            ram_usage_bytes: 0, // ToDo: Implement
            disk_usage_bytes: 0,  // ToDo: Implement
        }
    }
}