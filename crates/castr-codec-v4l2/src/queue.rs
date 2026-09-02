//! One V4L2 buffer queue (OUTPUT or CAPTURE): allocation, mapping, DMABUF
//! export, queue/dequeue bookkeeping and teardown. Pure bookkeeping over `Ops`,
//! so it is unit-tested with `FakeOps`.

use crate::ops::{Dequeued, Mapping, Ops};
use std::io;
use std::os::fd::OwnedFd;

pub struct Buffer {
    pub mapping: Mapping,
    /// DMABUF handle exported at allocation; unused for now, reserved for a
    /// zero-copy present path.
    pub dmabuf: Option<OwnedFd>,
    pub queued: bool,
}

pub struct Queue {
    pub buf_type: u32,
    pub buffers: Vec<Buffer>,
    streaming: bool,
}

impl Queue {
    pub fn new(buf_type: u32) -> Self {
        Self {
            buf_type,
            buffers: Vec::new(),
            streaming: false,
        }
    }

    pub fn allocate<O: Ops>(&mut self, ops: &mut O, count: u32, export: bool) -> io::Result<()> {
        let granted = ops.reqbufs(self.buf_type, count)?;
        self.buffers.clear();
        for index in 0..granted {
            let info = ops.querybuf(self.buf_type, index)?;
            let mapping = ops.mmap(info.length as usize, info.mem_offset)?;
            let dmabuf = if export {
                Some(ops.expbuf(self.buf_type, index)?)
            } else {
                None
            };
            self.buffers.push(Buffer {
                mapping,
                dmabuf,
                queued: false,
            });
        }
        Ok(())
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    pub fn stream_on<O: Ops>(&mut self, ops: &mut O) -> io::Result<()> {
        ops.streamon(self.buf_type)?;
        self.streaming = true;
        Ok(())
    }

    /// STREAMOFF returns every queued buffer to us without a DQBUF.
    pub fn stream_off<O: Ops>(&mut self, ops: &mut O) -> io::Result<()> {
        ops.streamoff(self.buf_type)?;
        self.streaming = false;
        for b in &mut self.buffers {
            b.queued = false;
        }
        Ok(())
    }

    pub fn free_slot(&self) -> Option<usize> {
        self.buffers.iter().position(|b| !b.queued)
    }

    pub fn in_flight(&self) -> usize {
        self.buffers.iter().filter(|b| b.queued).count()
    }

    pub fn queue<O: Ops>(
        &mut self,
        ops: &mut O,
        index: usize,
        bytesused: u32,
        timestamp_us: u64,
    ) -> io::Result<()> {
        let b = self
            .buffers
            .get_mut(index)
            .ok_or_else(|| io::Error::other("buffer index out of range"))?;
        if b.queued {
            return Err(io::Error::other(format!("buffer {index} already queued")));
        }
        let length = b.mapping.len() as u32;
        ops.qbuf(self.buf_type, index as u32, length, bytesused, timestamp_us)?;
        b.queued = true;
        Ok(())
    }

    pub fn dequeue<O: Ops>(&mut self, ops: &mut O) -> io::Result<Option<Dequeued>> {
        let Some(d) = ops.dqbuf(self.buf_type)? else {
            return Ok(None);
        };
        match self.buffers.get_mut(d.index as usize) {
            Some(b) => b.queued = false,
            None => {
                return Err(io::Error::other(format!(
                    "driver dequeued unknown buffer {}",
                    d.index
                )))
            }
        }
        Ok(Some(d))
    }

    pub fn release<O: Ops>(&mut self, ops: &mut O) -> io::Result<()> {
        if self.streaming {
            self.stream_off(ops)?;
        }
        self.buffers.clear(); // unmaps and closes DMABUF fds
        ops.reqbufs(self.buf_type, 0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeOps;
    use crate::sys::*;

    const OUT: u32 = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;

    #[test]
    fn allocate_requests_queries_maps_and_exports_each_buffer() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, true).unwrap();
        assert_eq!(q.buffers.len(), 2);
        assert!(q.buffers.iter().all(|b| b.dmabuf.is_some() && !b.queued));
        assert_eq!(
            ops.calls,
            vec![
                "reqbufs(10,2)",
                "querybuf(10,0)",
                "mmap(1048576,0)",
                "expbuf(10,0)",
                "querybuf(10,1)",
                "mmap(1048576,4096)",
                "expbuf(10,1)",
            ]
        );
    }

    #[test]
    fn allocate_honours_the_count_the_driver_grants() {
        let mut ops = FakeOps::new();
        ops.granted = Some(5);
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, false).unwrap();
        assert_eq!(q.buffers.len(), 5);
        assert!(q.buffers.iter().all(|b| b.dmabuf.is_none()));
    }

    #[test]
    fn queue_and_dequeue_track_in_flight_and_free_slots() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, false).unwrap();
        assert_eq!(q.free_slot(), Some(0));
        q.queue(&mut ops, 0, 100, 1_000).unwrap();
        assert_eq!(q.in_flight(), 1);
        assert_eq!(q.free_slot(), Some(1));
        q.queue(&mut ops, 1, 50, 2_000).unwrap();
        assert_eq!(q.free_slot(), None);
        assert!(q.dequeue(&mut ops).unwrap().is_none());
        ops.push_dequeue(
            OUT,
            Dequeued {
                index: 0,
                bytesused: 0,
                timestamp_us: 1_000,
                flags: 0,
            },
        );
        let d = q.dequeue(&mut ops).unwrap().unwrap();
        assert_eq!(d.index, 0);
        assert_eq!(q.in_flight(), 1);
        assert_eq!(q.free_slot(), Some(0));
        assert!(ops.calls.contains(&"qbuf(10,0,100,1000)".to_string()));
    }

    #[test]
    fn queueing_an_already_queued_buffer_is_an_error_not_a_kernel_call() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 1, false).unwrap();
        q.queue(&mut ops, 0, 1, 0).unwrap();
        let n = ops.calls.len();
        assert!(q.queue(&mut ops, 0, 1, 0).is_err());
        assert_eq!(ops.calls.len(), n);
    }

    #[test]
    fn stream_off_returns_all_buffers_and_release_frees_them() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, true).unwrap();
        q.stream_on(&mut ops).unwrap();
        q.queue(&mut ops, 0, 1, 0).unwrap();
        q.stream_off(&mut ops).unwrap();
        assert_eq!(q.in_flight(), 0);
        assert!(!q.is_streaming());
        q.release(&mut ops).unwrap();
        assert!(q.buffers.is_empty());
        assert_eq!(ops.calls.last().unwrap(), "reqbufs(10,0)");
    }

    #[test]
    fn release_while_streaming_stops_first() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 1, false).unwrap();
        q.stream_on(&mut ops).unwrap();
        q.release(&mut ops).unwrap();
        let tail: Vec<_> = ops.calls.iter().rev().take(2).cloned().collect();
        assert_eq!(tail, vec!["reqbufs(10,0)", "streamoff(10)"]);
    }
}
