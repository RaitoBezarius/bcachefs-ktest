extern crate libc;
use std::fs::{OpenOptions, create_dir_all};
use std::os::unix::fs::OpenOptionsExt;
use std::io::ErrorKind;
use std::path::Path;
use std::process;
mod lib;
use lib::{Ktestrc, read_lines, ktestrc_read, git_get_commit};

extern crate multimap;
use multimap::MultiMap;
use die::die;

fn get_subtests(test_path: &str) -> Vec<String> {
    let test_name = Path::new(test_path).file_stem();

    if let Some(test_name) = test_name {
        let test_name = test_name.to_string_lossy();

        let output = std::process::Command::new(&test_path)
            .arg("list-tests")
            .output()
            .expect(&format!("failed to execute process {:?} ", &test_path))
            .stdout;
        let output = String::from_utf8_lossy(&output);

        output
            .split_whitespace()
            .map(|i| format!("{}.{}", test_name, i))
            .collect()
    } else {
        Vec::new()
    }
}

fn lockfile_exists(rc: &Ktestrc, commit: &str, subtest: &str, create: bool) -> bool {
    fn test_or_create(lockfile: &Path, create: bool) -> bool {
        if !create {
            lockfile.exists()
        } else {
            let dir = lockfile.parent();
            let r = create_dir_all(dir.unwrap());

            if let Err(e) = r {
                if e.kind() != ErrorKind::AlreadyExists {
                    die!("error creating {:?}: {}", dir, e);
                }
            }

            let mut options = OpenOptions::new();
            options.write(true);
            options.custom_flags(libc::O_CREAT);
            options.open(lockfile).is_ok()
        }
    }

    let lockfile = rc.ci_output_dir.join(commit).join(subtest);
    let mut exists = test_or_create(&lockfile, create);

    if exists {
        let timeout = std::time::Duration::new(3600, 0);
        let now = std::time::SystemTime::now();
        let metadata = std::fs::metadata(&lockfile).unwrap();

        if metadata.len() == 0&&
           metadata.is_file() &&
           metadata.modified().unwrap() + timeout < now &&
           std::fs::remove_file(&lockfile).is_ok() {
            exists = test_or_create(&lockfile, create);
        }
    }

    exists
}

struct TestJob {
    branch:     String,
    commit:     String,
    age:        usize,
    test:       String,
    subtests:   Vec<String>,
}

fn branch_get_next_test_job(rc: &Ktestrc, repo: &git2::Repository,
                            branch: &str, test_path: &str) -> Option<TestJob> {
    let mut ret =  TestJob {
        branch:     branch.to_string(),
        commit:     String::new(),
        age:        0,
        test:       test_path.to_string(),
        subtests:   Vec::new(),
    };

    let subtests = get_subtests(test_path);

    let mut walk = repo.revwalk().unwrap();
    let reference = git_get_commit(&repo, branch.to_string());
    if reference.is_err() {
        eprintln!("branch {} not found", branch);
        return None;
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        eprintln!("Error walking {}: {}", branch, e);
        return None;
    }

    for commit in walk
            .filter_map(|i| i.ok())
            .filter_map(|i| repo.find_commit(i).ok()) {
        let commit = commit.id().to_string();
        ret.commit = commit.clone();

        for subtest in subtests.iter() {
            if !lockfile_exists(rc, &commit, subtest, false) {
                ret.subtests.push(subtest.to_string());
                if ret.subtests.len() > 20 {
                    break;
                }
            }
        }

        if !ret.subtests.is_empty() {
            return Some(ret);
        }

        ret.age += 1;
    }

    None
}

fn get_best_test_job(rc: &Ktestrc, repo: &git2::Repository,
                     branch_tests: &MultiMap<String, String>) -> Option<TestJob> {
    let mut ret: Option<TestJob> = None;

    for (branch, testvec) in branch_tests.iter_all() {
        for test in testvec {
            let job = branch_get_next_test_job(rc, repo, branch, test);

            if let Some(job) = job {
                match &ret {
                    Some(r) => if r.age > job.age { ret = Some(job) },
                    None    => ret = Some(job),
                }
            }
        }
    }

    ret
}

fn create_job_lockfiles(rc: &Ktestrc, mut job: TestJob) -> Option<TestJob> {
    job.subtests = job.subtests.iter()
        .filter(|i| lockfile_exists(rc, &job.commit, &i, true))
        .map(|i| i.to_string())
        .collect();

    if !job.subtests.is_empty() { Some(job) } else { None }
}

fn main() {
    let ktestrc = ktestrc_read();

    let repo = git2::Repository::open(&ktestrc.ci_linux_repo);
    if let Err(e) = repo {
        eprintln!("Error opening {:?}: {}", ktestrc.ci_linux_repo, e);
        eprintln!("Please specify correct JOBSERVER_LINUX_DIR");
        process::exit(1);
    }
    let repo = repo.unwrap();

    let lines = read_lines(&ktestrc.ci_branches_to_test);
    if let Err(e) = lines {
        eprintln!("Error opening {:?}: {}", ktestrc.ci_branches_to_test, e);
        eprintln!("Please specify correct JOBSERVER_BRANCHES_TO_TEST");
        process::exit(1);
    }
    let lines = lines.unwrap();

    let lines = lines.filter_map(|i| i.ok());

    let mut branch_tests: MultiMap<String, String> = MultiMap::new();

    for l in lines {
        let l: Vec<_> = l.split_whitespace().take(2).collect();

        if l.len() == 2 {
            let branch  = l[0];
            let test    = l[1];
            branch_tests.insert(branch.to_string(), test.to_string());
        }
    }

    let mut job: Option<TestJob>;

    loop {
        job = get_best_test_job(&ktestrc, &repo, &branch_tests);

        if job.is_none() {
            break;
        }

        job = create_job_lockfiles(&ktestrc, job.unwrap());
        if job.is_some() {
            break;
        }
    }

    if let Some(job) = job {
        print!("{} {} {}", job.branch, job.commit, job.test);
        for t in job.subtests {
            print!(" {}", t);
        }
        println!("");
    }
}
