use chaos_ipc::ProcessId;

pub fn get_process_id_from_citations(citations: Vec<String>) -> Vec<ProcessId> {
    let mut result = Vec::new();
    for citation in citations {
        let mut ids_block = None;
        for (open, close) in [
            ("<process_ids>", "</process_ids>"),
            ("<rollout_ids>", "</rollout_ids>"),
        ] {
            if let Some((_, rest)) = citation.split_once(open)
                && let Some((ids, _)) = rest.split_once(close)
            {
                ids_block = Some(ids);
                break;
            }
        }

        if let Some(ids_block) = ids_block {
            for id in ids_block
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                if let Ok(process_id) = ProcessId::try_from(id) {
                    result.push(process_id);
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::get_process_id_from_citations;
    use chaos_ipc::ProcessId;
    use pretty_assertions::assert_eq;

    #[test]
    fn get_process_id_from_citations_extracts_process_ids() {
        let first = ProcessId::new();
        let second = ProcessId::new();

        let citations = vec![format!(
            "<memory_citation>\n<citation_entries>\nMEMORY.md:1-2|note=[x]\n</citation_entries>\n<process_ids>\n{first}\nnot-a-uuid\n{second}\n</process_ids>\n</memory_citation>"
        )];

        assert_eq!(
            get_process_id_from_citations(citations),
            vec![first, second]
        );
    }

    #[test]
    fn get_process_id_from_citations_supports_legacy_rollout_ids() {
        let process_id = ProcessId::new();

        let citations = vec![format!(
            "<memory_citation>\n<rollout_ids>\n{process_id}\n</rollout_ids>\n</memory_citation>"
        )];

        assert_eq!(get_process_id_from_citations(citations), vec![process_id]);
    }
}
