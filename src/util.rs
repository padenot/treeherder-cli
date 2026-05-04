use crate::models::Job;
use anyhow::Result;
use notify_rust::Notification;

pub fn are_all_jobs_complete(jobs: &[Job]) -> bool {
    jobs.iter().all(|job| job.state == "completed")
}

pub fn count_job_states(jobs: &[Job]) -> (usize, usize, usize) {
    let completed = jobs.iter().filter(|j| j.state == "completed").count();
    let running = jobs.iter().filter(|j| j.state == "running").count();
    let pending = jobs.iter().filter(|j| j.state == "pending").count();
    (completed, running, pending)
}

pub fn send_notification(title: &str, message: &str) -> Result<()> {
    Notification::new().summary(title).body(message).show()?;
    Ok(())
}
