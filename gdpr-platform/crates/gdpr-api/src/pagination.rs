use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_page() -> u32 { 1 }
fn default_limit() -> u32 { 20 }

impl PaginationParams {
    pub fn limit(&self) -> u32 { self.limit.min(100) }
    pub fn offset(&self) -> u32 { (self.page.saturating_sub(1)) * self.limit() }
}

#[derive(Debug, Serialize)]
pub struct PagedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub meta: PageMeta,
}

#[derive(Debug, Serialize)]
pub struct PageMeta {
    pub total: u64,
    pub page:  u32,
    pub limit: u32,
}

impl<T: Serialize> PagedResponse<T> {
    pub fn new(data: Vec<T>, total: u64, params: &PaginationParams) -> Self {
        PagedResponse {
            data,
            meta: PageMeta {
                total,
                page:  params.page,
                limit: params.limit(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limit_clamped_at_100() {
        let p = PaginationParams { page: 1, limit: 500 };
        assert_eq!(p.limit(), 100);
    }

    #[test]
    fn test_offset_page_1() {
        let p = PaginationParams { page: 1, limit: 20 };
        assert_eq!(p.offset(), 0);
    }

    #[test]
    fn test_offset_page_3() {
        let p = PaginationParams { page: 3, limit: 20 };
        assert_eq!(p.offset(), 40);
    }

    #[test]
    fn test_paged_response_meta() {
        let p = PaginationParams { page: 2, limit: 10 };
        let r: PagedResponse<String> = PagedResponse::new(vec![], 100, &p);
        assert_eq!(r.meta.total, 100);
        assert_eq!(r.meta.page, 2);
        assert_eq!(r.meta.limit, 10);
    }
}
