package library;

public class Student extends Member {
    private final String major;

    public Student(String id, String name, String major) {
        super(id, name);
        this.major = major;
    }

    public String getMajor() {
        return major;
    }

    @Override
    public int maxCheckouts() {
        return 5;
    }
}
