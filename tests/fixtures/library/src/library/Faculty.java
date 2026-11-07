package library;

public class Faculty extends Member {
    private final String department;

    public Faculty(String id, String name, String department) {
        super(id, name);
        this.department = department;
    }

    public String getDepartment() {
        return department;
    }

    @Override
    public int maxCheckouts() {
        return 20;
    }
}
